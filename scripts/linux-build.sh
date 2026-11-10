#!/usr/bin/env bash
#------------------------------------------------------------------
# Linux 빌드 — 빌드서버(CentOS 7, glibc 2.17)에서 libdocparse.so 를 만들어 받아 온다
#=> 이 PC(Windows)에서 실행한다. 빌드서버에서 만들어야 오래된 리눅스(glibc 2.17)에서도 돈다.
#    1) 접속 정보 읽기 — scripts/server.env 의 SSH_ENV_FILE(KEY=VALUE 파일: SSH_SERVER·SSH_USER_ID·SSH_USER_PWD)
#    2) 소스 묶음(Cargo.toml·Cargo.lock·crates) 올리기
#    3) 빌드서버에서 cargo test --workspace · cargo build --release -p docparse-ffi
#       RPATH=$ORIGIN 이 새겨졌는지, 요구 glibc 가 2.17 이하인지 확인
#    4) libdocparse.so 를 받아 scripts/package.py 로 dist/docparse-<버전>-linux-x64.tgz
#
#   사용: bash scripts/linux-build.sh   (Git Bash · PuTTY 의 plink/pscp 필요)
#   설정: scripts/server.env (scripts/server.env.sample 참고, .gitignore 로 막혀 있다)
#------------------------------------------------------------------
set -euo pipefail
cd "$(dirname "$0")/.."

# 실패하면 사유를 남기고 멈춘다
die() { echo "[실패] $*" >&2; exit 1; }
# 단계 머리줄
step() { echo ""; echo "── $* ──"; }

[ -f scripts/server.env ] || die "scripts/server.env 가 없습니다 — scripts/server.env.sample 을 복사해 채우세요"
# KEY=VALUE 한 줄씩 읽어 환경변수로(주석·빈 줄 건너뜀, 윈도우 줄끝 제거)
while IFS='=' read -r k v; do
    case "$k" in ''|\#*) continue ;; esac
    export "$k=${v%$'\r'}"
done < scripts/server.env
: "${SSH_ENV_FILE:?server.env 에 SSH_ENV_FILE 이 없습니다}"
: "${DP_BUILD_DIR:?server.env 에 DP_BUILD_DIR 이 없습니다}"
: "${CARGO_BIN_DIR:?server.env 에 CARGO_BIN_DIR 이 없습니다}"
command -v plink >/dev/null || die "plink 를 찾을 수 없습니다 (PuTTY 설치·PATH 확인)"
command -v pscp  >/dev/null || die "pscp 를 찾을 수 없습니다"
[ -f "$SSH_ENV_FILE" ] || die "접속 정보 파일이 없습니다: $SSH_ENV_FILE"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

step "접속 정보 읽기"
python - "$SSH_ENV_FILE" "$TMP" <<'PY' || die "접속 정보를 읽지 못했습니다"
import io, sys
d = {}
for enc in ("utf-8", "cp949"):
    try:
        lines = io.open(sys.argv[1], encoding=enc).read().splitlines()
        break
    except UnicodeDecodeError:
        continue
for line in lines:
    line = line.strip()
    if "=" in line and not line.startswith("#"):
        k, v = line.split("=", 1)
        d[k.strip()] = v.strip().strip('"').strip("'")
for k in ("SSH_SERVER", "SSH_USER_ID", "SSH_USER_PWD"):
    if not d.get(k):
        raise SystemExit("%s 가 없습니다" % k)
io.open(sys.argv[2] + "/.pw", "w", encoding="utf-8", newline="").write(d["SSH_USER_PWD"])
io.open(sys.argv[2] + "/.host", "w", encoding="utf-8", newline="").write(
    "%s %s\n" % (d["SSH_SERVER"].split(":")[0], d["SSH_USER_ID"]))
PY
read -r HOST USER_ID < "$TMP/.host" || true
HOST="${HOST%$'\r'}"; USER_ID="${USER_ID%$'\r'}"
PW="$(cat "$TMP/.pw")"; PW="${PW%$'\r'}"
echo "  $USER_ID@$HOST (비밀번호는 출력하지 않습니다)"

# 버전 = 크레이트 버전 + 이 PC 의 git 해시. 빌드서버에는 .git 이 없어 환경변수로 넘긴다.
VER_CRATE="$(grep -m1 '^version' crates/docparse-ffi/Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
HASH="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
if [ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]; then HASH="$HASH-dirty"; fi
VERSION="$VER_CRATE+$HASH"

step "소스 올리기 ($VERSION)"
tar -czf "$TMP/dpsrc.tgz" Cargo.toml Cargo.lock crates || die "소스를 묶지 못했습니다"
plink -batch -ssh -l "$USER_ID" -pw "$PW" "$HOST" "mkdir -p $DP_BUILD_DIR" || die "빌드 폴더를 만들지 못했습니다"
pscp -batch -q -pw "$PW" "$TMP/dpsrc.tgz" "$USER_ID@$HOST:$DP_BUILD_DIR/" || die "소스 전송 실패"

step "빌드서버에서 빌드"
plink -batch -ssh -l "$USER_ID" -pw "$PW" "$HOST" "bash -s" <<EOF || die "빌드서버에서 실패했습니다 (위 출력 참고)"
set -e
export PATH="$CARGO_BIN_DIR:\$PATH"
export DOCPARSE_GIT="$HASH"
cd "$DP_BUILD_DIR"
rm -rf crates Cargo.toml Cargo.lock
tar xzf dpsrc.tgz
cargo test --workspace > test.log 2>&1 || { tail -30 test.log; echo "cargo test 실패 — $DP_BUILD_DIR/test.log"; exit 1; }
cargo build --release -p docparse-ffi > build.log 2>&1 || { tail -30 build.log; echo "cargo build 실패 — $DP_BUILD_DIR/build.log"; exit 1; }
f=target/release/libdocparse.so
[ -f \$f ] || { echo "빌드 결과가 없습니다: \$f"; exit 1; }
readelf -d \$f | grep -Eq '(RPATH|RUNPATH).*\\\$ORIGIN' || { echo "RPATH=\\\$ORIGIN 이 없습니다"; exit 1; }
# 요구하는 glibc 기호 버전 중 가장 높은 것 — 2.17 을 넘으면 CentOS 7 에서 못 뜬다.
g=\$(objdump -T \$f | grep -o 'GLIBC_[0-9.]*' | sort -t. -k2 -n -u | tail -1)
echo "  최고 요구 glibc: \$g"
echo "  \$(ls -la \$f | awk '{print \$5}') B"
EOF

step "받아 오기"
mkdir -p target/linux-x64
pscp -batch -q -pw "$PW" "$USER_ID@$HOST:$DP_BUILD_DIR/target/release/libdocparse.so" target/linux-x64/ \
    || die "libdocparse.so 내려받기 실패"

step "묶기"
python scripts/package.py linux-x64 target/linux-x64/libdocparse.so "$VERSION"
