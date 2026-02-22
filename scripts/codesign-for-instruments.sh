#!/usr/bin/env zsh
# Re-sign a (Rust) binary to allow debuggers like Instruments to attach to it.
# Source: https://cocoaphony.micro.blog/2022/10/29/solving-required-kernel.html
# See https://github.com/rust-lang/rust/issues/107033 too.
set -eu

BIN=$1

codesign -s - -v -f --entitlements =(echo -n '<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd"\>
<plist version="1.0">
    <dict>
        <key>com.apple.security.get-task-allow</key>
        <true/>
    </dict>
</plist>') "$BIN"
echo
codesign -dvvv --entitlements=- "$BIN"