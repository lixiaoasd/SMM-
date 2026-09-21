//! 最小 libiconv-2.dll 替身：仅导出 libintl-8.dll 依赖的两个符号。
//!
//! 本机没有真的 libiconv（MSYS2 仓库无此包，全盘搜索无果），而 windres 的
//! NLS 路径只在加载翻译时才用到字符集转换——英文消息根本走不到。
//! 因此返回"无法创建转换描述符 / 转换失败"即可让 libintl 优雅降级。

use std::os::raw::{c_char, c_void};

/// iconv_t libiconv_open(const char *tocode, const char *fromcode);
#[no_mangle]
pub extern "C" fn libiconv_open(_tocode: *const c_char, _fromcode: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

/// size_t libiconv(iconv_t cd, char **inbuf, size_t *inbytesleft,
///                 char **outbuf, size_t *outbytesleft);
/// 返回 (size_t)-1 表示转换失败，并置 errno=EINVAL。
#[no_mangle]
pub unsafe extern "C" fn libiconv(
    _cd: *mut c_void,
    _inbuf: *mut *mut c_char,
    _inleft: *mut usize,
    _outbuf: *mut *mut c_char,
    _outleft: *mut usize,
) -> usize {
    usize::MAX
}
