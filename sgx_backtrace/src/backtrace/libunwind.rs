// Copyright (C) 2017-2019 Baidu, Inc. All Rights Reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions
// are met:
//
//  * Redistributions of source code must retain the above copyright
//    notice, this list of conditions and the following disclaimer.
//  * Redistributions in binary form must reproduce the above copyright
//    notice, this list of conditions and the following disclaimer in
//    the documentation and/or other materials provided with the
//    distribution.
//  * Neither the name of Baidu, Inc., nor the names of its
//    contributors may be used to endorse or promote products derived
//    from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
// "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
// LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
// A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
// OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
// LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
// DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
// THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
// (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

//! Backtrace support using libunwind/gcc_s/etc APIs.
//!
//! This module contains the ability to unwind the stack using libunwind-style
//! APIs. Note that there's a whole bunch of implementations of the
//! libunwind-like API, and this is just trying to be compatible with most of
//! them all at once instead of being picky.
//!
//! The libunwind API is powered by `_Unwind_Backtrace` and is in practice very
//! reliable at generating a backtrace. It's not entirely clear how it does it
//! (frame pointers? eh_frame info? both?) but it seems to work!
//!
//! Most of the complexity of this module is handling the various platform
//! differences across libunwind implementations. Otherwise this is a pretty
//! straightforward Rust binding to the libunwind APIs.
//!
//! This is the default unwinding API for all non-Windows platforms currently.

use sgx_libc::c_void;

pub enum Frame {
    Raw(*mut uw::_Unwind_Context),
    Cloned {
        ip: *mut c_void,
        symbol_address: *mut c_void,
    },
}

// With a raw libunwind pointer it should only ever be access in a readonly
// threadsafe fashion, so it's `Sync`. When sending to other threads via `Clone`
// we always switch to a version which doesn't retain interior pointers, so we
// should be `Send` as well.
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    pub fn ip(&self) -> *mut c_void {
        let ctx = match *self {
            Frame::Raw(ctx) => ctx,
            Frame::Cloned { ip, .. } => return ip,
        };
        unsafe {
            uw::_Unwind_GetIP(ctx) as *mut c_void
        }
    }

    pub fn symbol_address(&self) -> *mut c_void {
        if let Frame::Cloned { symbol_address, .. } = *self {
            return symbol_address;
        }

        // It seems that on OSX `_Unwind_FindEnclosingFunction` returns a
        // pointer to... something that's unclear. It's definitely not always
        // the enclosing function for whatever reason. It's not entirely clear
        // to me what's going on here, so pessimize this for now and just always
        // return the ip.
        //
        // Note the `skip_inner_frames.rs` test is skipped on OSX due to this
        // clause, and if this is fixed that test in theory can be run on OSX!
        if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
            self.ip()
        } else {
            unsafe { uw::_Unwind_FindEnclosingFunction(self.ip()) }
        }
    }
}

impl Clone for Frame {
    fn clone(&self) -> Frame {
        Frame::Cloned {
            ip: self.ip(),
            symbol_address: self.symbol_address(),
        }
    }
}

#[inline(always)]
pub unsafe fn trace(mut cb: &mut dyn FnMut(&super::Frame) -> bool) {
    uw::_Unwind_Backtrace(trace_fn, &mut cb as *mut _ as *mut _);

    extern fn trace_fn(ctx: *mut uw::_Unwind_Context,
                       arg: *mut c_void) -> uw::_Unwind_Reason_Code {
        let cb = unsafe { &mut *(arg as *mut &mut dyn FnMut(&super::Frame) -> bool) };
        let cx = super::Frame {
            inner: Frame::Raw(ctx),
        };

        let mut bomb = crate::Bomb { enabled: true };
        let keep_going = cb(&cx);
        bomb.enabled = false;

        if keep_going {
            uw::_URC_NO_REASON
        } else {
            uw::_URC_FAILURE
        }
    }
}

/// Unwind library interface used for backtraces
///
/// Note that dead code is allowed as here are just bindings
/// iOS doesn't use all of them it but adding more
/// platform-specific configs pollutes the code too much
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(dead_code)]
mod uw {
    pub use self::_Unwind_Reason_Code::*;

    use sgx_libc::{c_void, uintptr_t};

    #[repr(C)]
    pub enum _Unwind_Reason_Code {
        _URC_NO_REASON = 0,
        _URC_FOREIGN_EXCEPTION_CAUGHT = 1,
        _URC_FATAL_PHASE2_ERROR = 2,
        _URC_FATAL_PHASE1_ERROR = 3,
        _URC_NORMAL_STOP = 4,
        _URC_END_OF_STACK = 5,
        _URC_HANDLER_FOUND = 6,
        _URC_INSTALL_CONTEXT = 7,
        _URC_CONTINUE_UNWIND = 8,
        _URC_FAILURE = 9, // used only by ARM EABI
    }

    pub enum _Unwind_Context {}

    pub type _Unwind_Trace_Fn =
            extern fn(ctx: *mut _Unwind_Context,
                      arg: *mut c_void) -> _Unwind_Reason_Code;

    extern {
        // No native _Unwind_Backtrace on iOS
        #[cfg(not(all(target_os = "ios", target_arch = "arm")))]
        pub fn _Unwind_Backtrace(trace: _Unwind_Trace_Fn,
                                 trace_argument: *mut c_void)
                    -> _Unwind_Reason_Code;

        // available since GCC 4.2.0, should be fine for our purpose
        #[cfg(all(not(all(target_os = "android", target_arch = "arm")),
                  not(all(target_os = "freebsd", target_arch = "arm")),
                  not(all(target_os = "linux", target_arch = "arm"))))]
        pub fn _Unwind_GetIP(ctx: *mut _Unwind_Context)
                    -> uintptr_t;

        #[cfg(all(not(target_os = "android"),
                  not(all(target_os = "freebsd", target_arch = "arm")),
                  not(all(target_os = "linux", target_arch = "arm"))))]
        pub fn _Unwind_FindEnclosingFunction(pc: *mut c_void)
            -> *mut c_void;
    }

    // On android, the function _Unwind_GetIP is a macro, and this is the
    // expansion of the macro. This is all copy/pasted directly from the
    // header file with the definition of _Unwind_GetIP.
    #[cfg(any(all(target_os = "android", target_arch = "arm"),
              all(target_os = "freebsd", target_arch = "arm"),
              all(target_os = "linux", target_arch = "arm")))]
    pub unsafe fn _Unwind_GetIP(ctx: *mut _Unwind_Context) -> uintptr_t {
        #[repr(C)]
        enum _Unwind_VRS_Result {
            _UVRSR_OK = 0,
            _UVRSR_NOT_IMPLEMENTED = 1,
            _UVRSR_FAILED = 2,
        }
        #[repr(C)]
        enum _Unwind_VRS_RegClass {
            _UVRSC_CORE = 0,
            _UVRSC_VFP = 1,
            _UVRSC_FPA = 2,
            _UVRSC_WMMXD = 3,
            _UVRSC_WMMXC = 4,
        }
        #[repr(C)]
        enum _Unwind_VRS_DataRepresentation {
            _UVRSD_UINT32 = 0,
            _UVRSD_VFPX = 1,
            _UVRSD_FPAX = 2,
            _UVRSD_UINT64 = 3,
            _UVRSD_FLOAT = 4,
            _UVRSD_DOUBLE = 5,
        }

        type _Unwind_Word = sgx_libc::c_uint;
        extern {
            fn _Unwind_VRS_Get(ctx: *mut _Unwind_Context,
                               klass: _Unwind_VRS_RegClass,
                               word: _Unwind_Word,
                               repr: _Unwind_VRS_DataRepresentation,
                               data: *mut c_void)
                -> _Unwind_VRS_Result;
        }

        let mut val: _Unwind_Word = 0;
        let ptr = &mut val as *mut _Unwind_Word;
        let _ = _Unwind_VRS_Get(ctx, _Unwind_VRS_RegClass::_UVRSC_CORE, 15,
                                _Unwind_VRS_DataRepresentation::_UVRSD_UINT32,
                                ptr as *mut c_void);
        (val & !1) as uintptr_t
    }

    // This function also doesn't exist on Android or ARM/Linux, so make it
    // a no-op
    #[cfg(any(target_os = "android",
              all(target_os = "freebsd", target_arch = "arm"),
              all(target_os = "linux", target_arch = "arm")))]
    pub unsafe fn _Unwind_FindEnclosingFunction(pc: *mut c_void)
        -> *mut c_void
    {
        pc
    }
}


