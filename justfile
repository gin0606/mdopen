app := "target/mdo.app"

# 版は git tag が唯一の真実。`just version=X.Y.Z ...` で明示的に上書きできる。
# just の変数は環境変数から上書きされないので、VERSION が他の用途で export されて
# いる環境でも黙って乗っ取られない。
# 厳密一致で引くのは、直近の tag を拾うと、その後ろのコミットのビルドが released 版
# と同じ版を名乗ってしまうため。
# x.y.z の形だけを版として認め、それ以外の tag は版タグ無しと同じ扱いにする。
# git の ref 名は `"` や `$(` を含められるので、形を見ずに通すと、そういう tag の
# 載ったコミットで just を動かしただけで任意のコマンドが走る。
version := `v=$(git describe --tags --exact-match --match 'v[0-9]*' 2>/dev/null || true); v=${v#v}; if printf '%s' "$v" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then echo "$v"; else echo 0.0.0; fi`

default: check

check: fmt-check lint test

fmt-check:
    cargo fmt --check

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

# 版は sed と shell に素通しで埋まるので、形をここで確かめる。
# 見張る相手は `just version=...` で手渡された値。git から引いた版は上で形を
# 絞ってあるので、ここに届く時点で既に x.y.z か 0.0.0 になっている。
# quote を通してから shell 変数に受けるのは、検査する前の値を shell の語として
# 展開すると、検査より先にその中身が実行されるため。quote が返すのは単引用符付きの
# 値なので、それをさらに引用符の中に置くと引用が無効になり同じ穴が開く。
[private]
check-version:
    #!/usr/bin/env bash
    set -euo pipefail
    version={{quote(version)}}
    if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      echo "版 '$version' が x.y.z の形をしていません" >&2
      exit 1
    fi

# CLI をリリースビルドする
build: check-version
    MDO_VERSION={{quote(version)}} cargo build --release

# CLI と Swift ランチャを組み合わせて mdo.app を組み立てる
build-app: build
    rm -rf {{app}}
    mkdir -p {{app}}/Contents/MacOS
    swiftc -O -o {{app}}/Contents/MacOS/mdo-launcher macos/Launcher.swift
    sed 's/@VERSION@/{{version}}/g' macos/Info.plist.in > {{app}}/Contents/Info.plist
    cp target/release/mdo {{app}}/Contents/MacOS/mdo
