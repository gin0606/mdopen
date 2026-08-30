app := "target/mdo.app"

# 版は git tag が唯一の真実。`just version=X.Y.Z ...` で明示的に上書きできる。
# just の変数は環境変数から上書きされないので、VERSION が他の用途で export されて
# いる環境でも黙って乗っ取られない。
# 厳密一致で引くのは、直近の tag を拾うと未 tag のコミットのビルドが released 版と
# 同じ版・同じ配布物名を名乗ってしまうため。
# x.y.z の形だけを版として認め、それ以外の tag は版タグ無しと同じ扱いにする。
# git の ref 名は `"` や `$(` を含められるので、形を見ずに通すと、そういう tag の
# 載ったコミットで just を動かしただけで任意のコマンドが走る。
version := `v=$(git describe --tags --exact-match --match 'v[0-9]*' 2>/dev/null || true); v=${v#v}; if printf '%s' "$v" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then echo "$v"; else echo 0.0.0; fi`

# 配布するのは arm64 のみ。配布物の名前・cargo の target・swiftc の三つ組を
# すべてここから引くので、組んだものと名乗る名前が食い違わない。
arch := "arm64"
rust_target := if arch == "arm64" { "aarch64-apple-darwin" } else if arch == "x86_64" { "x86_64-apple-darwin" } else { error("arch は arm64 か x86_64 のどちらか") }
app_dist := "target/mdo.app-" + version + "-macos-" + arch + ".zip"
cli_dist := "target/mdo-" + version + "-macos-" + arch + ".tar.gz"

# 配布物は Developer ID で署名する。手元には鍵が無いので、識別名が渡されなければ
# ad-hoc 署名に落とす。version と同じ理由で環境変数は見ない。
sign_identity := "-"
# 公証は hardened runtime と署名時刻の両方を要求する。
codesign_options := if sign_identity == "-" { "" } else { "--options runtime --timestamp" }

default: check

check: fmt-check lint test

fmt-check:
    cargo fmt --check

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

# 対応 macOS 版は Info.plist の宣言が唯一の真実。cargo も swiftc も既定では
# ビルドしたマシンの版を下限にするので、両方にこの値を渡す。
# 変数ではなくレシピなのは、just が変数の backtick をどのレシピの実行前にも評価
# するため。変数にすると cargo test まで macOS 専用のコマンドを要求する。
[private]
macos-min:
    @plutil -extract LSMinimumSystemVersion raw -o - macos/Info.plist.in

[doc("CLI をリリースビルドする")]
build: check-version
    #!/usr/bin/env bash
    set -euo pipefail
    # 代入の前置は、値を作るコマンドが失敗しても行を止めない (空のまま cargo が走り、
    # rustc の既定である 11.0 が黙って下限になる)。先に変数へ取って set -e に見せる。
    macos_min=$({{just_executable()}} macos-min)
    MDO_VERSION={{quote(version)}} MACOSX_DEPLOYMENT_TARGET="$macos_min" cargo build --release --target {{rust_target}}

# 同梱の CLI は bundle の主実行ファイルではないため、bundle への署名では署名され
# ない。識別子を明示するのは、省略すると ad-hoc 署名だけハッシュ付きの名前になり、
# 手元と CI で署名の中身がずれるため。
[doc("CLI と Swift ランチャを組み合わせて mdo.app を組み立て、署名する")]
build-app: build
    rm -rf {{app}}
    mkdir -p {{app}}/Contents/MacOS
    swiftc -O -target {{arch}}-apple-macos$({{just_executable()}} macos-min) -o {{app}}/Contents/MacOS/mdo-launcher macos/Launcher.swift
    sed 's/@VERSION@/{{version}}/g' macos/Info.plist.in > {{app}}/Contents/Info.plist
    cp target/{{rust_target}}/release/mdo {{app}}/Contents/MacOS/mdo
    codesign --force {{codesign_options}} -i me.gin0606.mdo.cli --sign {{quote(sign_identity)}} {{app}}/Contents/MacOS/mdo
    codesign --force {{codesign_options}} --sign {{quote(sign_identity)}} {{app}}
    codesign --verify --strict {{app}}

# 実機を持たずに確かめられるのは宣言と実物の一致までで、これを外すと「入るのに
# 起動しない」や「版の埋め込みが効いていない」が誰にも気づかれずに配られる。
[doc("組み上げた bundle が、配布物の名前と Info.plist の宣言どおりか確かめる")]
check-app: build-app
    #!/usr/bin/env bash
    set -euo pipefail
    version={{quote(version)}}
    for key in CFBundleShortVersionString CFBundleVersion; do
      baked=$(plutil -extract "$key" raw -o - {{app}}/Contents/Info.plist)
      if [ "$baked" != "$version" ]; then
        echo "Info.plist の $key が $baked で、ビルドした版 $version と違います" >&2
        exit 1
      fi
    done
    reported=$({{app}}/Contents/MacOS/mdo --version)
    if [ "$reported" != "mdo $version" ]; then
      echo "同梱の CLI が '$reported' と名乗り、ビルドした版 $version と違います" >&2
      exit 1
    fi
    declared=$(plutil -extract LSMinimumSystemVersion raw -o - {{app}}/Contents/Info.plist)
    for binary in {{app}}/Contents/MacOS/*; do
      archs=$(lipo -archs "$binary")
      if [ "$archs" != "{{arch}}" ]; then
        echo "$binary は $archs 向けで、配布物が名乗る {{arch}} と違います" >&2
        exit 1
      fi
      minos=$(vtool -show-build-version "$binary" | awk '/minos/ { print $2 }')
      if [ -z "$minos" ]; then
        echo "$binary から minos を読み取れません" >&2
        exit 1
      fi
      if [ "$(printf '%s\n%s\n' "$minos" "$declared" | sort -V | head -1)" != "$minos" ]; then
        echo "$binary の minos ($minos) が LSMinimumSystemVersion ($declared) を超えています" >&2
        exit 1
      fi
    done
    echo "版 $version / {{arch}} / macOS $declared 以上"

# 版は sed と shell に素通しで埋まり、配布物の名前にもなる。形をここで確かめる。
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

# 版タグの無いコミット (0.0.0) と汚れた作業ツリーからの配布物を止める。
# `just version=X.Y.Z` で手渡された版は、それが tag と対応するかまでは見ていない。
# リリースの workflow は起動した tag から版を取るのでそこは一致するが、手で叩けば
# tag の無い版の配布物も作れる。
[private]
check-releasable: check-version
    #!/usr/bin/env bash
    set -euo pipefail
    version={{quote(version)}}
    if [ "$version" = "0.0.0" ]; then
      echo "版タグの無いコミットからは配布物を作れません (just version=X.Y.Z で明示する)" >&2
      exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
      echo "作業ツリーが汚れています。配布物は commit された状態からしか作れません" >&2
      exit 1
    fi

[doc("配布物を両方作る")]
dist: check dist-app dist-cli

[doc(".app の配布物を作る")]
dist-app: check-releasable check-app
    rm -f {{app_dist}}
    ditto -c -k --keepParent {{app}} {{app_dist}}
    @shasum -a 256 {{app_dist}}

# .app に同梱したものと同じ実体を取り出すので、2 つの配布経路に別々にビルドした
# ものが流れることがない。
# COPYFILE_DISABLE は、bsdtar が拡張属性を ._ ファイルとして書庫に混ぜるのを止める。
[doc("CLI 単体の配布物を作る")]
dist-cli: check-releasable check-app
    rm -f {{cli_dist}}
    COPYFILE_DISABLE=1 tar -czf {{cli_dist}} -C {{app}}/Contents/MacOS mdo
    @shasum -a 256 {{cli_dist}}

# 公証は Apple のサーバとやりとりするので、鍵を持つリリースの workflow からしか
# 通せない。前提はビルドより先に見る。zip まで作ってから鍵の不足で落ちると、
# そこまでの数分が無駄になる。空の secret を復号しても 0 バイトのファイルは
# 残るので、鍵は中身の有無まで見る。
[private]
check-notary:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ {{quote(sign_identity)}} = "-" ]; then
      echo "公証には Developer ID の署名が要ります (just sign_identity=... で渡す)" >&2
      exit 1
    fi
    if [ ! -s "${NOTARY_KEY:-}" ]; then
      echo "NOTARY_KEY が指す鍵ファイルが空か存在しません" >&2
      exit 1
    fi
    if [ -z "${NOTARY_KEY_ID:-}" ] || [ -z "${NOTARY_ISSUER_ID:-}" ]; then
      echo "NOTARY_KEY_ID / NOTARY_ISSUER_ID を環境変数で渡してください" >&2
      exit 1
    fi

# staple 前の zip を配ると、Gatekeeper が初回起動のたびに Apple へ問い合わせに行く。
# チケットを綴じてから zip を作り直す。spctl はチケットが無くてもオンラインの照会で
# 通してしまうので、綴じられたことは stapler validate の側で見る。
[private]
notarize: check-notary dist-app
    xcrun notarytool submit {{app_dist}} --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" --wait --timeout 30m
    xcrun stapler staple {{app}}
    rm -f {{app_dist}}
    ditto -c -k --keepParent {{app}} {{app_dist}}
    codesign --verify --strict {{app}}
    xcrun stapler validate {{app}}
    spctl -a -vv -t exec {{app}}
    @echo "staple 済みの配布物:"
    @shasum -a 256 {{app_dist}}

# 1 回の just で通すのは、間に build-app を挟み直すと bundle が組み直されて、
# 綴じたチケットも .app の zip も無効になるため。
[doc("公証まで済ませた配布物を両方作る")]
release: check-notary check notarize dist-cli

# リリースの workflow が配布物の在処を読む
[private]
dist-app-path:
    @echo {{app_dist}}

[private]
dist-cli-path:
    @echo {{cli_dist}}
