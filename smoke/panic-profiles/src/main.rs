use debugbundle::{Client,Config,delivery::{Transport,TransportFailure,TransportResponse}};
use serde_json::json;
use std::{sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
struct Sink(Arc<AtomicUsize>);
impl Transport for Sink{
    fn send(&self,body:&[u8],_:Duration)->Result<TransportResponse,TransportFailure>{
        let value:serde_json::Value=serde_json::from_slice(body).map_err(|_|TransportFailure)?;let count=value["events"].as_array().ok_or(TransportFailure)?.len();self.0.fetch_add(count,Ordering::SeqCst);
        Ok(TransportResponse{status:202,body:Some(json!({"accepted":count,"rejected":0,"errors":[]})),retry_after:None})
    }
}
fn main(){
    let previous=Arc::new(AtomicUsize::new(0));let observed=previous.clone();std::panic::set_hook(Box::new(move|_|{observed.fetch_add(1,Ordering::SeqCst);eprintln!("previous-hook");}));
    let hooks=Arc::new(AtomicUsize::new(0));let observed=hooks.clone();let received=Arc::new(AtomicUsize::new(0));
    let client=Client::new(Config{transport:Some(Arc::new(Sink(received.clone()))),before_send:Some(Arc::new(move|event|{observed.fetch_add(1,Ordering::SeqCst);Some(event)})),..Config::new("synthetic","panic-smoke")});let registration=client.install_panic_observer();assert!(registration.is_active());
    if std::env::args().nth(1).as_deref()==Some("abort"){panic!("synthetic abort profile");}
    assert!(std::panic::catch_unwind(||panic!("synthetic unwind profile")).is_err());assert!(client.flush(Duration::from_secs(2)).drained);assert_eq!(received.load(Ordering::SeqCst),1);assert_eq!(previous.load(Ordering::SeqCst),1);assert_eq!(hooks.load(Ordering::SeqCst),0);println!("unwind-survived");
}
