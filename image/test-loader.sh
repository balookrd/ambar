#!/usr/bin/env bash
# Regression test for image/load-module.sh: runs every branch of the loader
# against a fake sysfs and fake uname/modprobe/insmod. Needs no Docker and no
# Linux kernel. Compatible with bash 3.2 (no associative arrays, no mapfile).
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
loader="$here/load-module.sh"
kver="6.1.0-test"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Fake executables shared by all cases. insmod simulates the kernel by
# creating the sysfs entries a real load would produce.
mkdir -p "$work/bin"
cat > "$work/bin/uname" <<FAKE
#!/usr/bin/env bash
[[ "\${1:-}" == "-r" ]] && echo "$kver"
FAKE
cat > "$work/bin/modprobe" <<'FAKE'
#!/usr/bin/env bash
echo "modprobe $*" >> "$CALL_LOG"
FAKE
cat > "$work/bin/insmod" <<'FAKE'
#!/usr/bin/env bash
echo "insmod $*" >> "$CALL_LOG"
case "$(basename "$1")" in
    drbd.ko)
        mkdir -p "$SYS_MODULE_DIR/drbd"
        echo 9.3.4 > "$SYS_MODULE_DIR/drbd/version"
        ;;
    drbd_transport_tcp.ko)
        mkdir -p "$SYS_MODULE_DIR/drbd_transport_tcp"
        ;;
esac
FAKE
chmod +x "$work/bin/uname" "$work/bin/modprobe" "$work/bin/insmod"

failed=0

# run_case <name> <setup> <want_rc> <want_stdout> <want_stderr> <want_calls>
# <setup> is a space-separated list of fixture items:
#   drbd9 | drbd8 | transport  - modules already loaded (sysfs entries)
#   ko-drbd | ko-transport     - .ko files present in the modules dir
# <want_calls> uses "|" between expected modprobe/insmod invocations.
run_case() {
    local name="$1" setup="$2" want_rc="$3" want_out="$4" want_err="$5" want_calls="$6"
    local root="$work/case" item rc diffs="" calls
    rm -rf "$root"
    mkdir -p "$root/sys" "$root/modules/$kver"
    : > "$root/calls"

    for item in $setup; do
        case "$item" in
            drbd9) mkdir -p "$root/sys/drbd"; echo 9.3.4 > "$root/sys/drbd/version" ;;
            drbd8) mkdir -p "$root/sys/drbd"; echo 8.4.11 > "$root/sys/drbd/version" ;;
            transport) mkdir -p "$root/sys/drbd_transport_tcp" ;;
            ko-drbd) : > "$root/modules/$kver/drbd.ko" ;;
            ko-transport) : > "$root/modules/$kver/drbd_transport_tcp.ko" ;;
        esac
    done

    PATH="$work/bin:$PATH" CALL_LOG="$root/calls" SYS_MODULE_DIR="$root/sys" \
        MODULES_DIR="$root/modules" \
        "$loader" > "$root/out" 2> "$root/err"
    rc=$?

    calls="$(tr '\n' '|' < "$root/calls")"
    calls="${calls%|}"
    # Normalize the per-case fixture path out of the recorded arguments.
    calls="${calls//$root\/modules\/$kver/<dir>}"

    [[ "$rc" == "$want_rc" ]] || diffs="$diffs exit=$rc (want $want_rc);"
    [[ "$(cat "$root/out")" == "$want_out" ]] || diffs="$diffs stdout='$(cat "$root/out")' (want '$want_out');"
    [[ "$(cat "$root/err")" == "$want_err" ]] || diffs="$diffs stderr='$(cat "$root/err")' (want '$want_err');"
    [[ "$calls" == "$want_calls" ]] || diffs="$diffs calls='$calls' (want '$want_calls');"

    if [[ -z "$diffs" ]]; then
        echo "ok   $name"
    else
        echo "FAIL $name:$diffs"
        failed=1
    fi
}

run_case "9.x loaded, transport present" \
    "drbd9 transport" 0 \
    "drbd 9.3.4 is already loaded" "" ""

run_case "9.x loaded, transport missing, ko present" \
    "drbd9 ko-transport" 0 \
    "drbd 9.3.4 is already loaded" "" \
    "insmod <dir>/drbd_transport_tcp.ko"

run_case "9.x loaded, transport missing, ko missing" \
    "drbd9" 1 \
    "" "no drbd_transport_tcp module for kernel $kver" ""

run_case "in-tree 8.x loaded" \
    "drbd8 ko-drbd ko-transport" 1 \
    "" "in-tree drbd 8.4 is loaded (version 8.4.11)" ""

run_case "nothing loaded, drbd.ko missing" \
    "" 1 \
    "" "no drbd module for kernel $kver" ""

run_case "nothing loaded, full load" \
    "ko-drbd ko-transport" 0 \
    "loaded drbd 9.3.4 for kernel $kver" "" \
    "modprobe -a lru_cache libcrc32c|insmod <dir>/drbd.ko usermode_helper=disabled|insmod <dir>/drbd_transport_tcp.ko"

if [[ "$failed" -ne 0 ]]; then
    exit 1
fi
echo "OK: 6 loader cases"
