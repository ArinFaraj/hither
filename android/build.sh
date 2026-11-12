#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
PLATFORM=$(ls -d "$SDK"/platforms/android-* | sort -V | tail -1)
JAR="$PLATFORM/android.jar"
BT=$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)
KEY="${HITHER_KEYSTORE:-$HOME/.android/debug.keystore}"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -1)
IFS=. read -r MAJ MIN PAT <<< "${VERSION%%-*}"
CODE=$((MAJ * 1000000 + MIN * 1000 + PAT))
if [ ! -f "$KEY" ]; then
  mkdir -p "$(dirname "$KEY")"
  keytool -genkeypair -keystore "$KEY" -storepass android -keypass android -alias androiddebugkey \
    -dname "CN=Android Debug,O=Android,C=US" -keyalg RSA -keysize 2048 -validity 10000
fi
rm -rf build && mkdir -p build/res build/classes build/dex
"$BT/aapt2" compile --dir res -o build/res.zip
"$BT/aapt2" link -I "$JAR" --manifest AndroidManifest.xml --java build/gen \
  --min-sdk-version 33 --target-sdk-version 35 --version-code "$CODE" --version-name "$VERSION" \
  -o build/unsigned.apk build/res.zip
javac -nowarn --release 17 -cp "$JAR" -d build/classes $(find src build/gen -name '*.java')
"$BT/d8" --release --min-api 33 --lib "$JAR" --output build/dex $(find build/classes -name '*.class')
(cd build/dex && zip -q -j ../unsigned.apk classes.dex)
"$BT/zipalign" -f -p 4 build/unsigned.apk build/aligned.apk
"$BT/apksigner" sign --ks "$KEY" --ks-pass "pass:${HITHER_KEYSTORE_PASS:-android}" --out build/hither.apk build/aligned.apk
cp build/hither.apk ../assets/hither.apk
echo ../assets/hither.apk
