# LCOV contains executable lines. Require a receipt for every compiled SDK source
# file; tests, dependency sources and generated build files are not SDK modules.
function finish() {
    if (source in expected) {
        files++
        seen[source] = 1
        if (lines == 0 || hits / lines < 0.8) {
            printf "%s: %.2f%% (%d/%d) below 80%%\n", source, lines ? 100 * hits / lines : 0, hits, lines > "/dev/stderr"
            failed = 1
        }
    }
}
NR == FNR { expected["/work/" $0] = 1; next }
/^SF:/ { source = substr($0, 4); lines = 0; hits = 0 }
/^LF:/ { lines = substr($0, 4) + 0 }
/^LH:/ { hits = substr($0, 4) + 0 }
/^end_of_record/ { finish(); source = "" }
END {
    for (path in expected) {
        if (!(path in seen)) {
            printf "%s: missing production coverage record\n", path > "/dev/stderr"
            failed = 1
        }
    }
    if (files == 0) { print "No SDK source coverage records" > "/dev/stderr"; exit 1 }
    if (failed) exit 1
    printf "Per-file coverage passed for %d SDK modules\n", files
}
