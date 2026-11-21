# Android port

This is the native Android front end for Mister Download Manager. Kotlin/Compose
owns the UI and a foreground service; the Rust `mdm-engine` runs inside the app
through the `mdm-android` JNI library. No desktop UI or browser connection is
built for Android.

## What works in this MVP

- HTTP and HTTPS downloads, including resume after pause or process restart.
- Per-download connection count (1–16), progress, speed, and pause/resume/remove.
- Files are saved under the app's own Downloads folder
  (`getExternalFilesDir(DIRECTORY_DOWNLOADS)`), so no broad storage permission
  is needed.
- A foreground service with a progress notification keeps the process alive.
- HTTPS uses Android's system trust store through `rustls-platform-verifier`.

Torrents, debrid sites, the browser extension, and other desktop features are
not part of this port yet.

## Building

The GitHub Actions workflow `.github/workflows/android.yml` builds the three
native libraries with `cargo-ndk`, then assembles `app-debug.apk` with Gradle and
uploads it as an artifact.

To build locally you need the Android SDK (platform 35, build tools 35, NDK
27.2.12479018), JDK 17, the Rust Android targets, `cargo-ndk`, and Gradle 8.9:

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk --locked
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o android/app/src/main/jniLibs build --release -p mdm-android
gradle -p android assembleDebug
```

The APK is at `android/app/build/outputs/apk/debug/app-debug.apk`. It is unsigned
by Android's debug key, which is enough to install on a phone with USB debugging
or via `adb install -r`.
