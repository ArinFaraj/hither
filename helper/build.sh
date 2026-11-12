#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
JAR=$(ls -d "$SDK"/platforms/android-*/android.jar | sort -V | tail -1)
BT=$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)
rm -rf build && mkdir -p build/stubs build/classes
javac -nowarn --release 11 -cp "$JAR" -d build/stubs stubs/android/content/*.java
javac -nowarn --release 11 -cp "$JAR:build/stubs" -d build/classes src/hither/*.java
"$BT/d8" --release --min-api 26 --lib "$JAR" --classpath build/stubs --output build/helper.jar $(find build/classes -name '*.class')
cp build/helper.jar ../assets/hither-helper.jar
echo ../assets/hither-helper.jar
