//! Native Android bridge for MDM's download engine.
//!
//! Kotlin owns the UI and the foreground service; this crate owns one Tokio
//! runtime and the engine's `Download` handles. Every call comes in over JNI on
//! a JVM thread; progress goes back through a `DownloadCallback` global ref.
//!
//! The engine's resume state (`<file>.mdm.json`) and sparse `.part` file are
//! reused as-is, so a paused download survives a process restart when Kotlin
//! restores its metadata and calls `nativeAdd` again.

// JNI handles are raw pointers by design. Nothing else in this crate needs unsafe.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jni::JValue;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JObject, JString};
use jni::refs::{Global, Reference};
use jni::strings::JNIString;
use jni::sys::{jint, jlong};
use jni::vm::JavaVM;
use jni::EnvUnowned;
use mdm_engine::download::{Destination, Download, Options, Request, Snapshot, State};
use reqwest::header::HeaderMap;
use serde_json::json;
use url::Url;

/// A mobile User-Agent. The desktop engine's automatic agent reports "Linux"
/// because Android is a Linux target; a phone should identify as a phone.
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 14; Mobile) \
                         AppleWebKit/537.36 (KHTML, like Gecko) \
                         Chrome/131.0.0.0 Mobile Safari/537.36 MisterDownloadManager/0.1";

struct Entry {
    download: Arc<Download>,
    dest: PathBuf,
    url: String,
    connections: i32,
    last_downloaded: u64,
    user_paused: bool,
}

struct StateStore {
    downloads: HashMap<i64, Arc<Mutex<Entry>>>,
    callback: Option<Global<JObject<'static>>>,
}

struct AndroidEngine {
    vm: JavaVM,
    runtime: tokio::runtime::Runtime,
    store: Arc<Mutex<StateStore>>,
}

impl AndroidEngine {
    fn new(vm: JavaVM) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("mdm-android-engine")
            .enable_all()
            .build()
            .expect("MDM: cannot start the Tokio runtime");
        Self {
            vm,
            runtime,
            store: Arc::new(Mutex::new(StateStore {
                downloads: HashMap::new(),
                callback: None,
            })),
        }
    }

    /// Start (or resume) one download with the caller's id. Kotlin owns the id
    /// so it can persist it and restore the same list after a process restart.
    fn start(
        &self,
        id: i64,
        url: &str,
        dest: &Path,
        connections: i32,
        shown: u64,
    ) -> Result<(), String> {
        let parsed = Url::parse(url).map_err(|e| format!("bad URL: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("only http and https links are supported in this build".to_owned());
        }

        let mut opts = Options::default();
        opts.max_connections = connections.clamp(1, 16) as u32;
        opts.automatic_agent = false;
        opts.user_agent = USER_AGENT.to_owned();
        opts.save_interval = Duration::from_secs(1);

        let req = Request {
            url: parsed,
            headers: HeaderMap::new(),
            dest: Destination::File(dest.to_owned()),
        };
        let state_file = mdm_engine::state::state_path(dest);

        let download = self
            .runtime
            .block_on(async { Download::resume_or_start(state_file, req, opts, None, shown) });
        let download = Arc::new(download);

        let entry = Arc::new(Mutex::new(Entry {
            download: download.clone(),
            dest: dest.to_owned(),
            url: url.to_owned(),
            connections,
            last_downloaded: shown,
            user_paused: false,
        }));

        // Replacing a live entry (a restore with the same id) stops its old
        // monitor. `Arc::ptr_eq` in `monitor` keeps the old task from touching
        // the replacement.
        if let Some(old) = self.store.lock().unwrap().downloads.insert(id, entry.clone()) {
            old.lock().unwrap().download.cancel();
        }

        let store = self.store.clone();
        let vm = self.vm.clone();
        self.runtime.spawn(monitor(id, download, entry, store, vm));
        Ok(())
    }

    fn cancel_all(&self) {
        let mut store = self.store.lock().unwrap();
        for entry in store.downloads.values() {
            entry.lock().unwrap().download.cancel();
        }
        store.downloads.clear();
    }
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Probing => "Probing",
        State::Downloading => "Downloading",
        State::Verifying => "Verifying",
        State::Merging => "Merging",
        State::Completing => "Completing",
        State::Stopping => "Paused",
        State::Done => "Done",
        State::Failed => "Failed",
        State::Cancelled => "Cancelled",
    }
}

fn callback_of(store: &Mutex<StateStore>) -> Option<Global<JObject<'static>>> {
    store.lock().unwrap().callback.clone()
}

/// Call a Kotlin callback from a Rust worker thread. The thread is attached for
/// the duration of the call; local refs are released when the closure ends.
fn emit_progress(vm: &JavaVM, store: &Mutex<StateStore>, id: i64, snap: &Snapshot) {
    let Some(cb) = callback_of(store) else { return };
    let name = state_name(snap.state).to_owned();
    let total = snap.size.unwrap_or(0);
    let _ = vm.attach_current_thread(move |env| -> jni::errors::Result<()> {
        let name = env.new_string(name)?;
        env.call_method(
            &cb,
            jni::jni_str!("onProgress"),
            jni::jni_sig!((jlong, jlong, jlong, java.lang.String) -> void),
            &[
                JValue::Long(id as jlong),
                JValue::Long(snap.downloaded as jlong),
                JValue::Long(total as jlong),
                JValue::Object(&name),
            ],
        )?;
        Ok(())
    });
}

fn emit_finished(vm: &JavaVM, cb: Global<JObject<'static>>, id: i64, path: &Path, size: u64) {
    let path = path.to_string_lossy().into_owned();
    let _ = vm.attach_current_thread(move |env| -> jni::errors::Result<()> {
        let path = env.new_string(path)?;
        env.call_method(
            &cb,
            jni::jni_str!("onFinished"),
            jni::jni_sig!((jlong, java.lang.String, jlong) -> void),
            &[
                JValue::Long(id as jlong),
                JValue::Object(&path),
                JValue::Long(size as jlong),
            ],
        )?;
        Ok(())
    });
}

fn emit_failed(vm: &JavaVM, cb: Global<JObject<'static>>, id: i64, message: String) {
    let _ = vm.attach_current_thread(move |env| -> jni::errors::Result<()> {
        let message = env.new_string(message)?;
        env.call_method(
            &cb,
            jni::jni_str!("onFailed"),
            jni::jni_sig!((jlong, java.lang.String) -> void),
            &[JValue::Long(id as jlong), JValue::Object(&message)],
        )?;
        Ok(())
    });
}

fn emit_paused(vm: &JavaVM, cb: Global<JObject<'static>>, id: i64) {
    let _ = vm.attach_current_thread(move |env| -> jni::errors::Result<()> {
        env.call_method(
            &cb,
            jni::jni_str!("onPaused"),
            jni::jni_sig!((jlong) -> void),
            &[JValue::Long(id as jlong)],
        )?;
        Ok(())
    });
}

async fn monitor(
    id: i64,
    download: Arc<Download>,
    entry: Arc<Mutex<Entry>>,
    store: Arc<Mutex<StateStore>>,
    vm: JavaVM,
) {
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let snap = download.snapshot();

        {
            let mut e = entry.lock().unwrap();
            e.last_downloaded = snap.downloaded;
        }
        emit_progress(&vm, &store, id, &snap);

        let terminal = matches!(snap.state, State::Done | State::Failed | State::Cancelled);
        if !terminal {
            continue;
        }

        // A newer entry with this id (a manual restore/resume) owns the id now.
        let is_current = store
            .lock()
            .unwrap()
            .downloads
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, &entry));
        if !is_current {
            break;
        }

        let user_paused = entry.lock().unwrap().user_paused;
        let cb = callback_of(&store);
        match (snap.state, cb) {
            (State::Done, Some(cb)) => emit_finished(
                &vm,
                cb,
                id,
                snap.file.as_deref().unwrap_or(Path::new("")),
                snap.size.unwrap_or(snap.downloaded),
            ),
            (State::Failed, Some(cb)) => emit_failed(
                &vm,
                cb,
                id,
                snap.error.clone().unwrap_or_else(|| "Download failed".to_owned()),
            ),
            (State::Cancelled, Some(cb)) if user_paused => emit_paused(&vm, cb, id),
            _ => {}
        }

        // A paused download stays in the store so `nativeResume` can restart it.
        if !user_paused || snap.state != State::Cancelled {
            store.lock().unwrap().downloads.remove(&id);
        }
        break;
    }
}

fn snapshot_json(id: i64, snap: Snapshot, entry: &Entry) -> serde_json::Value {
    json!({
        "id": id,
        "url": entry.url,
        "path": entry.dest.to_string_lossy(),
        "state": state_name(snap.state),
        "downloaded": snap.downloaded,
        "total": snap.size,
        "speed": snap.connections.iter().map(|c| c.speed).sum::<u64>(),
        "error": snap.error,
    })
}

fn engine_from(handle: jlong) -> &'static AndroidEngine {
    // The handle is owned by Kotlin's `nativeDestroy` and stays alive while any
    // native call is in flight (the repository serializes access to it).
    unsafe { &*(handle as *const AndroidEngine) }
}

fn bad_url(env: &mut jni::Env<'_>, message: &str) -> jni::errors::Result<()> {
    let msg = JNIString::new(message);
    env.throw_new(jni::jni_str!("java/lang/IllegalArgumentException"), &msg)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeInit<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    _data_dir: JString<'caller>,
    context: JObject<'caller>,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        #[cfg(target_os = "android")]
        rustls_platform_verifier::android::init_with_env(env, context)?;
        #[cfg(not(target_os = "android"))]
        let _ = context;

        let vm = env.get_java_vm()?;
        Ok(Box::into_raw(Box::new(AndroidEngine::new(vm))) as jlong)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeSetCallback<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    callback: JObject<'caller>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let engine = engine_from(handle);
        let global = env.new_global_ref(callback)?;
        engine.store.lock().unwrap().callback = Some(global);
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeAdd<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    id: jlong,
    url: JString<'caller>,
    dest: JString<'caller>,
    connections: jint,
    shown: jlong,
) -> jlong {
    env.with_env(|env| -> jni::errors::Result<jlong> {
        let url = url.try_to_string(env)?;
        let dest = PathBuf::from(dest.try_to_string(env)?);
        let engine = engine_from(handle);

        if let Err(message) = engine.start(id, &url, &dest, connections, shown as u64) {
            bad_url(env, &message)?;
            return Ok(0);
        }
        Ok(id)
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativePause<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    id: jlong,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        let engine = engine_from(handle);
        let entry = engine.store.lock().unwrap().downloads.get(&id).cloned();
        if let Some(entry) = entry {
            let mut entry = entry.lock().unwrap();
            entry.user_paused = true;
            entry.download.cancel();
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeResume<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    id: jlong,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let engine = engine_from(handle);
        let old = engine.store.lock().unwrap().downloads.remove(&id);
        let Some(old) = old else { return Ok(()) };
        let (url, dest, connections, shown, paused) = {
            let entry = old.lock().unwrap();
            (
                entry.url.clone(),
                entry.dest.clone(),
                entry.connections,
                entry.last_downloaded,
                entry.user_paused,
            )
        };
        if !paused {
            // It was still running; put it back untouched.
            engine.store.lock().unwrap().downloads.insert(id, old);
            return Ok(());
        }
        if let Err(message) = engine.start(id, &url, &dest, connections, shown) {
            bad_url(env, &message)?;
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeRemove<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    id: jlong,
) {
    env.with_env(|_env| -> jni::errors::Result<()> {
        let engine = engine_from(handle);
        let entry = engine.store.lock().unwrap().downloads.remove(&id);
        if let Some(entry) = entry {
            let mut entry = entry.lock().unwrap();
            entry.user_paused = false;
            entry.download.cancel();
        }
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeSnapshot<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
    id: jlong,
) -> jni::sys::jstring {
    env.with_env(|env| -> jni::errors::Result<jni::sys::jstring> {
        let engine = engine_from(handle);
        let entry = engine.store.lock().unwrap().downloads.get(&id).cloned();
        let value = match entry {
            Some(entry) => {
                let e = entry.lock().unwrap();
                let snap = e.download.snapshot();
                snapshot_json(id, snap, &e)
            }
            None => json!({ "id": id, "state": "Missing" }),
        };
        let text = env.new_string(value.to_string())?;
        Ok(text.into_raw())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeList<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) -> jni::sys::jstring {
    env.with_env(|env| -> jni::errors::Result<jni::sys::jstring> {
        let engine = engine_from(handle);
        let entries: Vec<_> = engine.store.lock().unwrap().downloads.clone();
        let mut items = Vec::new();
        for (id, entry) in entries {
            let e = entry.lock().unwrap();
            let snap = e.download.snapshot();
            items.push(snapshot_json(id, snap, &e));
        }
        let text = env.new_string(json!(items).to_string())?;
        Ok(text.into_raw())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_misterdownloadmanager_android_DownloadEngine_nativeDestroy<'caller>(
    mut env: EnvUnowned<'caller>,
    _this: JObject<'caller>,
    handle: jlong,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        if handle == 0 {
            return Ok(());
        }
        let engine = unsafe { Box::from_raw(handle as *mut AndroidEngine) };
        engine.cancel_all();
        // Drop the global ref here, on a JVM thread, before the box goes away.
        let _ = env;
        engine.store.lock().unwrap().callback.take();
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}
