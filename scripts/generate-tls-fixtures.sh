#!/bin/sh
# Synthetic loopback-only certificate material; never install in a host trust store.
set -eu
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 \
  -keyout root.key -out root.pem -subj '/CN=DebugBundle synthetic test root' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:1' -addext 'keyUsage=critical,keyCertSign,cRLSign' 2>/dev/null
openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout intermediate.key \
  -out intermediate.csr -subj '/CN=DebugBundle synthetic intermediate' 2>/dev/null
cat > intermediate.ext <<'EOF'
basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,keyCertSign,cRLSign
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid,issuer
EOF
openssl x509 -req -in intermediate.csr -CA root.pem -CAkey root.key -set_serial 2 \
  -days 3650 -extfile intermediate.ext -out intermediate.pem 2>/dev/null
openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -keyout server.key \
  -out server.csr -subj '/CN=localhost' 2>/dev/null
cat > leaf.ext <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid,issuer
EOF
openssl x509 -req -in server.csr -CA intermediate.pem -CAkey intermediate.key -set_serial 3 \
  -days 3650 -extfile leaf.ext -out server.pem 2>/dev/null
touch index
printf '04\n' > serial
cat > ca.cnf <<'EOF'
[ca]
default_ca=test
[test]
database=index
new_certs_dir=.
serial=serial
certificate=intermediate.pem
private_key=intermediate.key
default_md=sha256
policy=policy
[policy]
commonName=supplied
EOF
openssl ca -batch -config ca.cnf -in server.csr -startdate 200101000000Z -enddate 210101000000Z \
  -extfile leaf.ext -out expired.pem 2>/dev/null
for name in root intermediate server expired; do
  openssl x509 -in "$name.pem" -outform DER -out "$name.der"
done
openssl pkcs8 -topk8 -nocrypt -in server.key -outform DER -out server-key.der
# JSON keeps the fixture inside the package's reviewed canonical-fixture inventory.
python3 - <<'PY'
import base64, json
from pathlib import Path
names = ['root', 'intermediate', 'server', 'expired', 'server-key']
data = {name: base64.b64encode(Path(name + '.der').read_bytes()).decode() for name in names}
data['purpose'] = 'Synthetic loopback TLS tests only. No production trust or credentials.'
Path('/work/tests/fixtures/tls-certificates.json').write_text(json.dumps(data, indent=2) + '\n')
PY
