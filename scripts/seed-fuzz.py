"""Seed bounded targets from the vendored language-neutral contracts."""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parent.parent
def seed(target, data):
    if not isinstance(data, bytes):
        data = json.dumps(data, ensure_ascii=False, separators=(",", ":")).encode()
    if len(data) > 65536:
        return
    directory = root / "fuzz/corpus" / target
    directory.mkdir(parents=True, exist_ok=True)
    (directory / hashlib.sha256(data).hexdigest()).write_bytes(data)

evidence = json.loads((root / "tests/fixtures/evidence-wire-cases.json").read_text())
for case in evidence["events"]:
    seed("wire", case["event"])
for case in evidence["capabilities"]:
    seed("wire", case["input"])
for case in evidence["json_sizes"]:
    seed("wire", case["value"])
for fixture in ("analytics-wire-cases.json", "privacy-conformance.json"):
    value = json.loads((root / "tests/fixtures" / fixture).read_text())
    for case in value.get("events", value.get("cases", [])):
        seed("wire", case.get("event", case.get("input")))
for count in (0, 1, 2, 100, 101):
    seed("wire", {"count": count, "ack": {"accepted": count, "rejected": 0, "errors": []}})
for reason in ("rate_limited", "monthly_quota_exceeded", "invalid_event"):
    seed("wire", {"count": 2, "ack": {"accepted": 1, "rejected": 1, "errors": [{"index": 1, "reason": reason}]}})
relay = json.loads((root / "tests/fixtures/relay-compliance.json").read_text())
for case in relay["cases"]:
    body = case.get("request", {}).get("bodyJson")
    if body is not None:
        data = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode()
        for mode in range(8):
            seed("relay", bytes([mode]) + data)
for data in (b"", b"\x00", bytes(range(256)), b"012301230123", b"password=SYNTHETIC_SECRET", "Ω stack\n 0: worker::run\n at src/main.rs:42:7".encode()):
    seed("snapshots", data)
for data in (b"rust-service", b"../escape", b"/absolute", b"..", b"CON", b"name:stream", b"a\x00b", "COM¹".encode(), b"x"*256):
    seed("files", data)
print("Prepared contract seeds for wire, snapshot, relay and private-file targets")
