pub mod app;

pub use app::run as run_app;

// Android native activity entry point. Exported when building the cdylib for APK.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn android_main(app: ndk_glue::android_activity::AndroidApp) {
    let _guard = ndk_glue::init(app);
    let _ = crate::run_app();
}
