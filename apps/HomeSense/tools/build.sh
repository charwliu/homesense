#!/bin/bash
set -euo pipefail
project_dir="$(cd "$(dirname "$0")/.." && pwd)"
platform="${1:-macos}"
case "$platform" in
    macos) destination='generic/platform=macOS'; signing=(CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual) ;;
    ios) destination='generic/platform=iOS'; signing=(CODE_SIGNING_ALLOWED=NO) ;;
    simulator) destination='generic/platform=iOS Simulator'; signing=(CODE_SIGNING_ALLOWED=NO) ;;
    *) echo 'Usage: bash tools/build.sh [macos|ios|simulator]' >&2; exit 2 ;;
esac
xcodebuild -project "$project_dir/HomeSense.xcodeproj" -scheme HomeSense \
    -destination "$destination" -configuration Debug \
    -derivedDataPath "$project_dir/build/$platform" \
    "${signing[@]}" build
