#!/usr/bin/env bash
#------------------------------------------------------------------
# Windows 빌드 — 이 PC 에서 docparse.dll 을 만들고 배포 묶음(zip)으로 낸다
#=> 1) cargo test --workspace (본체·C 진입점 시험)
#   2) cargo build --release -p docparse-ffi → target/release/docparse.dll
#   3) Python 감싸개로 불러 보기(ABI·버전·텍스트 한 건 추출)
#   4) scripts/package.py 로 dist/docparse-<버전>-windows-x64.zip
#
#   사용: bash scripts/win-build.sh   (Git Bash)
#------------------------------------------------------------------
set -euo pipefail
cd "$(dirname "$0")/.."

# 실패하면 사유를 남기고 멈춘다
die() { echo "[실패] $*" >&2; exit 1; }
# 단계 머리줄
step() { echo ""; echo "── $* ──"; }

command -v cargo >/dev/null || die "cargo 를 찾을 수 없습니다"

# 버전 = 크레이트 버전 + git 해시(커밋 안 된 수정이 있으면 -dirty). build.rs 와 같은 규칙이다.
VER_CRATE="$(grep -m1 '^version' crates/docparse-ffi/Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
HASH="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
if [ -n "$(git status --porcelain --untracked-files=no 2>/dev/null)" ]; then HASH="$HASH-dirty"; fi
VERSION="$VER_CRATE+$HASH"
export DOCPARSE_GIT="$HASH"

step "시험"
cargo test --workspace >/dev/null 2>&1 || die "cargo test 실패 — 'cargo test --workspace' 로 보세요"

step "릴리스 빌드 ($VERSION)"
cargo build --release -p docparse-ffi >/dev/null 2>&1 || die "cargo build 실패"
LIB=target/release/docparse.dll
[ -f "$LIB" ] || die "빌드 결과가 없습니다: $LIB"

step "불러 보기"
cp third_party/pdfium/windows-x64/pdfium.dll target/release/
tmp="$(mktemp -d)"
printf '문서파서 시험 본문입니다.\n' > "$tmp/a.txt"
DOCPARSE_LIB="$PWD/$LIB" PYTHONIOENCODING=utf-8 python -I - "$PWD/bindings/python" "$tmp/a.txt" "$VERSION" <<'PY' \
    || die "Python 감싸개로 불러 보기 실패"
import sys
sys.path.insert(0, sys.argv[1])
import docparse
assert docparse.version() == sys.argv[3], (docparse.version(), sys.argv[3])
r = docparse.extract(sys.argv[2], clean=True)
assert "시험" in r.text, r
print("  ", docparse.version(), r)
PY
rm -rf "$tmp"

step "묶기"
python scripts/package.py windows-x64 "$LIB" "$VERSION"
