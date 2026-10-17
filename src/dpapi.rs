//! Windows DPAPI 封装（CryptProtectData / CryptUnprotectData）。
//!
//! 密码加密原语：`protect` 把明文密码加密为当前用户可解的 blob（CurrentUser scope），
//! `unprotect` 反向解密。手写 FFI，不引入任何 Windows 相关 crate，保住零依赖构建。
//!
//! 两个安全约束：
//! - entropy 固定为 `b"roq-profile-v1"`：把密文绑定到 roq 配置用途，
//!   其他程序/其他用途产生的 DPAPI 密文即使同用户也无法被 roq 解开（反之亦然）；
//! - `CRYPTPROTECT_UI_FORBIDDEN`：CLI 无界面，禁止 DPAPI 弹出任何提示窗口。
//!
//! 仅 Windows 提供（DPAPI 是 Windows 独有机制）：cfg 门在 lib.rs 的
//! `#[cfg(windows)] pub mod dpapi;` 声明处，非 Windows 平台整个模块不参与编译，
//! 保证后续接线后三平台 CI 构建不因链接 DPAPI 失败。

use std::ffi::c_void;
use std::io::Error as IoError;
use std::ptr;

/// entropy：绑定到 roq 配置文件格式 v1，防 DPAPI 密文跨用途复用。
const ENTROPY: &[u8] = b"roq-profile-v1";

/// 禁止 DPAPI 弹 UI（MSDN CRYPTPROTECT_UI_FORBIDDEN，值 0x1）。
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

/// MSDN `DATA_BLOB`（cryptprotect.h 的 CRYPT_INTEGER_BLOB）。
#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

extern "system" {
    fn CryptProtectData(
        pdata_in: *const DataBlob,
        sz_data_descr: *const u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *const c_void,
        p_prompt_struct: *const c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        p_data_in: *const DataBlob,
        ppsz_data_descr: *mut *mut u16,
        p_optional_entropy: *const DataBlob,
        pv_reserved: *const c_void,
        p_prompt_struct: *const c_void,
        dw_flags: u32,
        p_data_out: *mut DataBlob,
    ) -> i32;
    fn LocalFree(hmem: *mut c_void) -> *mut c_void;
}

/// DPAPI 出参 blob 的 RAII 守卫：native 内存归 LocalFree 管，
/// 之后任何路径（含 `?` 与早退 return）离开作用域都会释放，防泄漏。
struct OwnedBlob(DataBlob);

impl Drop for OwnedBlob {
    fn drop(&mut self) {
        if !self.0.pb_data.is_null() {
            // SAFETY: pb_data 由 DPAPI 通过 LocalAlloc 分配，本结构保证只释放一次
            unsafe { LocalFree(self.0.pb_data.cast::<c_void>()) };
        }
    }
}

impl OwnedBlob {
    fn new_uninit() -> Self {
        OwnedBlob(DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        })
    }

    /// 把出参 blob 复制成 Rust 侧 Vec<u8>（native 内存随后由守卫释放）。
    fn to_vec(&self) -> Vec<u8> {
        // SAFETY: DPAPI 成功返回后 pb_data/cb_data 描述一块有效的 native 缓冲区
        unsafe { std::slice::from_raw_parts(self.0.pb_data, self.0.cb_data as usize) }.to_vec()
    }
}

/// 加密明文（如数据库密码），返回仅当前 Windows 用户可解的 blob。
pub fn protect(plain: &str) -> Result<Vec<u8>, String> {
    let input = DataBlob {
        cb_data: plain.len() as u32,
        pb_data: plain.as_ptr() as *mut u8,
    };
    let entropy = DataBlob {
        cb_data: ENTROPY.len() as u32,
        pb_data: ENTROPY.as_ptr() as *mut u8,
    };
    let mut out = OwnedBlob::new_uninit();
    let ok = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out.0,
        )
    };
    if ok == 0 {
        return Err(format!("DPAPI 加密失败：{}", IoError::last_os_error()));
    }
    Ok(out.to_vec())
}

/// 解密 [`protect`] 产生的 blob，返回原始字节（密码的 UTF-8 编码）。
pub fn unprotect(blob: &[u8]) -> Result<Vec<u8>, String> {
    let input = DataBlob {
        cb_data: blob.len() as u32,
        pb_data: blob.as_ptr() as *mut u8,
    };
    let entropy = DataBlob {
        cb_data: ENTROPY.len() as u32,
        pb_data: ENTROPY.as_ptr() as *mut u8,
    };
    let mut out = OwnedBlob::new_uninit();
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out.0,
        )
    };
    if ok == 0 {
        return Err(format!("DPAPI 解密失败：{}", IoError::last_os_error()));
    }
    Ok(out.to_vec())
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_protect_unprotect() {
        assert_eq!(
            unprotect(&protect("s3cret-密码").unwrap()).unwrap(),
            "s3cret-密码".as_bytes()
        );
    }

    #[test]
    fn roundtrip_empty_plain() {
        assert_eq!(unprotect(&protect("").unwrap()).unwrap(), b"");
    }

    #[test]
    fn unprotect_garbage_blob_fails_cleanly() {
        assert!(unprotect(b"garbage").is_err());
    }

    #[test]
    fn unprotect_tampered_blob_fails() {
        let mut blob = protect("s3cret-密码").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(unprotect(&blob).is_err());
    }
}
