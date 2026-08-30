app := "target/mdo.app"

# CLI をリリースビルドする
build:
    cargo build --release

# CLI と Swift ランチャを組み合わせて mdo.app を組み立てる
build-app: build
    rm -rf {{app}}
    mkdir -p {{app}}/Contents/MacOS
    swiftc -O -o {{app}}/Contents/MacOS/mdo-launcher macos/Launcher.swift
    cp macos/Info.plist {{app}}/Contents/Info.plist
    cp target/release/mdo {{app}}/Contents/MacOS/mdo

test:
    cargo test
