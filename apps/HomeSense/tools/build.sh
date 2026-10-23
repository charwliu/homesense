#!/bin/bash
set -euo pipefail
project_dir="$(cd "$(dirname "$0")/.." && pwd)"
workspace_root="$(cd "$project_dir/../.." && pwd)"
build_root="${HOMESENSE_BUILD_ROOT:-$workspace_root/target}"
case "$build_root" in
    /*) ;;
    *) echo 'HOMESENSE_BUILD_ROOT must be an absolute path' >&2; exit 2 ;;
esac
platform="${1:-macos}"
case "$platform" in
    macos) destination='generic/platform=macOS'; signing=(CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual) ;;
    ios) destination='generic/platform=iOS'; signing=(CODE_SIGNING_ALLOWED=NO) ;;
    simulator) destination='generic/platform=iOS Simulator'; signing=(CODE_SIGNING_ALLOWED=NO) ;;
    *) echo 'Usage: bash tools/build.sh [macos|ios|simulator]' >&2; exit 2 ;;
esac
xcodebuild -project "$project_dir/HomeSense.xcodeproj" -scheme HomeSense \
    -destination "$destination" -configuration Debug \
    -derivedDataPath "$build_root/$platform" \
    "${signing[@]}" build
