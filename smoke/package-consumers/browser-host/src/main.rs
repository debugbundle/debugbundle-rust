use debugbundle::{
    relay::{Relay, RelayOptions},
    Client, Context, ExceptionSnapshot,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

struct Fixture {
    bundle: String,
    html: String,
    done: tokio::sync::Notify,
    receipt: Mutex<Option<Value>>,
    client: Client,
    mock: Arc<artifact_support::Mock>,
}
impl Fixture {
    fn finish(&self, receipt: Value) {
        *self.receipt.lock().unwrap() = Some(receipt);
        self.done.notify_one();
    }
    fn failure(&self, context: &Context) {
        self.client.capture_exception(
            &ExceptionSnapshot::new("BrowserBackendFailure", "synthetic backend failure"),
            context,
        );
    }
}

#[actix_web::main]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 4, "framework, bundle, HTML, result");
    let mock = Arc::new(artifact_support::Mock::start());
    let directory = tempfile::tempdir().unwrap();
    let client = mock.client("artifact-browser-host", directory.path().join("spool"));
    mock.wait_config(&client);
    let relay = Relay::new(client.clone(), RelayOptions::default());
    let fixture = Arc::new(Fixture {
        bundle: std::fs::read_to_string(&args[1]).unwrap(),
        html: std::fs::read_to_string(&args[2]).unwrap(),
        done: tokio::sync::Notify::new(),
        receipt: Mutex::new(None),
        client: client.clone(),
        mock: mock.clone(),
    });
    let server = match args[0].as_str() {
        "axum" => Server::axum(fixture.clone(), relay.clone()).await,
        "actix" => Server::actix(fixture.clone(), relay.clone()),
        _ => panic!("Unknown framework"),
    };
    println!("{}", json!({"origin":server.origin()}));
    tokio::time::timeout(Duration::from_secs(60), fixture.done.notified())
        .await
        .unwrap();
    assert!(client.flush_async(Duration::from_secs(5)).await.drained);
    assert!(relay.flush_async(Duration::from_secs(5)).await);
    let result = json!({"framework":args[0],"page":fixture.receipt.lock().unwrap().clone(),"events":mock.events()});
    std::fs::write(
        PathBuf::from(&args[3]),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    assert!(client.shutdown_async(Duration::from_secs(1)).await.drained);
    server.stop().await;
}

enum Server {
    Axum(String, tokio::task::JoinHandle<()>),
    Actix(
        String,
        actix_web::dev::ServerHandle,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ),
}
impl Server {
    fn origin(&self) -> &str {
        match self {
            Self::Axum(origin, _) | Self::Actix(origin, _, _) => origin,
        }
    }
    async fn stop(self) {
        match self {
            Self::Axum(_, task) => {
                task.abort();
                let _ = task.await;
            }
            Self::Actix(_, handle, task) => {
                handle.stop(true).await;
                task.await.unwrap().unwrap();
            }
        }
    }
    async fn axum(fixture: Arc<Fixture>, relay: Relay) -> Self {
        use axum::{
            extract::{Extension, State},
            http::{header, StatusCode},
            routing::{get, post},
            Json, Router,
        };
        async fn page(
            State(f): State<Arc<Fixture>>,
        ) -> ([(header::HeaderName, &'static str); 1], String) {
            ([(header::CONTENT_TYPE, "text/html")], f.html.clone())
        }
        async fn bundle(
            State(f): State<Arc<Fixture>>,
        ) -> ([(header::HeaderName, &'static str); 1], String) {
            (
                [(header::CONTENT_TYPE, "text/javascript")],
                f.bundle.clone(),
            )
        }
        async fn done(State(f): State<Arc<Fixture>>, Json(receipt): Json<Value>) -> &'static str {
            f.finish(receipt);
            "ok"
        }
        async fn observed(State(f): State<Arc<Fixture>>) -> Json<Vec<Value>> {
            Json(f.mock.events())
        }
        async fn failure(
            State(f): State<Arc<Fixture>>,
            Extension(context): Extension<Context>,
        ) -> StatusCode {
            f.failure(&context);
            StatusCode::INTERNAL_SERVER_ERROR
        }
        let app = Router::new()
            .route("/checkout", get(page))
            .route("/sdk.js", get(bundle))
            .route("/done", post(done))
            .route("/observed", get(observed))
            .route("/failure", get(failure))
            .with_state(fixture.clone())
            .merge(debugbundle::axum::browser_relay(relay))
            .layer(debugbundle::axum::DebugBundleLayer::new(
                fixture.client.clone(),
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self::Axum(origin, task)
    }
    fn actix(fixture: Arc<Fixture>, relay: Relay) -> Self {
        use actix_web::{web, App, HttpMessage, HttpRequest, HttpResponse, HttpServer};
        use std::{cell::Cell, rc::Rc};
        async fn page(f: web::Data<Arc<Fixture>>) -> HttpResponse {
            HttpResponse::Ok()
                .content_type("text/html")
                .body(f.html.clone())
        }
        async fn bundle(f: web::Data<Arc<Fixture>>) -> HttpResponse {
            HttpResponse::Ok()
                .content_type("text/javascript")
                .body(f.bundle.clone())
        }
        async fn done(f: web::Data<Arc<Fixture>>, receipt: web::Json<Value>) -> HttpResponse {
            f.finish(receipt.into_inner());
            HttpResponse::Ok().finish()
        }
        async fn observed(f: web::Data<Arc<Fixture>>) -> web::Json<Vec<Value>> {
            web::Json(f.mock.events())
        }
        async fn failure(
            f: web::Data<Arc<Fixture>>,
            request: HttpRequest,
            local: web::Data<Rc<Cell<usize>>>,
        ) -> HttpResponse {
            local.set(local.get() + 1);
            f.failure(request.extensions().get::<Context>().unwrap());
            HttpResponse::InternalServerError().finish()
        }
        let server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(fixture.clone()))
                .app_data(web::Data::new(Rc::new(Cell::new(0usize))))
                .wrap(debugbundle::actix_web::DebugBundleMiddleware::new(
                    fixture.client.clone(),
                ))
                .route("/checkout", web::get().to(page))
                .route("/sdk.js", web::get().to(bundle))
                .route("/done", web::post().to(done))
                .route("/observed", web::get().to(observed))
                .route("/failure", web::get().to(failure))
                .service(debugbundle::actix_web::browser_relay(relay.clone()))
        })
        .workers(2)
        .bind(("127.0.0.1", 0))
        .unwrap();
        let origin = format!("http://{}", server.addrs()[0]);
        let server = server.run();
        let handle = server.handle();
        Self::Actix(origin, handle, actix_web::rt::spawn(server))
    }
}
