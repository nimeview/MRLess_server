use jni::objects::{JClass, JObject, JString};
use jni::sys::{jint, jshort};
use jni::JNIEnv;
use std::ffi::CString;

#[no_mangle]
pub extern "system" fn Java_local_mrless_app_NativeNode_start<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    data_dir: JString<'local>,
    name: JObject<'local>,
) -> jint {
    let dir = match env.get_string(&data_dir) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let dir_c = match CString::new(dir.to_string_lossy().as_bytes()) {
        Ok(c) => c,
        Err(_) => return -1,
    };
    let name_c = if name.is_null() {
        None
    } else {
        let js = JString::from(name);
        env.get_string(&js)
            .ok()
            .and_then(|s| CString::new(s.to_string_lossy().as_bytes()).ok())
    };
    let name_ptr = name_c
        .as_ref()
        .map(|c| c.as_ptr())
        .unwrap_or(std::ptr::null());
    crate::ffi::mrless_start(dir_c.as_ptr(), name_ptr)
}

#[no_mangle]
pub extern "system" fn Java_local_mrless_app_NativeNode_stop<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    crate::ffi::mrless_stop();
}

#[no_mangle]
pub extern "system" fn Java_local_mrless_app_NativeNode_wsPort<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jshort {
    crate::ffi::mrless_ws_port() as jshort
}

#[no_mangle]
pub extern "system" fn Java_local_mrless_app_NativeNode_meshPort<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jshort {
    crate::ffi::mrless_mesh_port() as jshort
}

#[no_mangle]
pub extern "system" fn Java_local_mrless_app_NativeNode_reconnectPeers<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jint {
    crate::ffi::mrless_reconnect_peers()
}
