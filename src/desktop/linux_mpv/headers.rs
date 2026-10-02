//! Opaque HTTP fields cross libmpv as an array, without option-list syntax.
use std::{ffi::CString, os::raw::c_void};

pub(super) struct HeaderList {
    fields: Vec<CString>,
    count: i32,
}

impl HeaderList {
    pub(super) fn new(fields: Vec<String>) -> crate::Result<Self> {
        let count = i32::try_from(fields.len())
            .map_err(|_| crate::Error::InvalidRequest("too many HTTP fields".into()))?;
        let fields = fields
            .into_iter()
            .map(CString::new)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| crate::Error::InvalidRequest("invalid HTTP field".into()))?;
        Ok(Self { fields, count })
    }
}

// SAFETY: Format::Node matches the C mpv_node passed to the synchronous setter.
// All CString buffers, the node array and its list remain alive during the
// callback. The C API reads and copies these caller-owned values; Rust retains
// ownership, so no mpv_free_node_contents call is appropriate.
unsafe impl libmpv2::SetData for HeaderList {
    fn get_format() -> libmpv2::Format {
        libmpv2::Format::Node
    }

    fn call_as_c_void<T, F: FnMut(*mut c_void) -> libmpv2::Result<T>>(
        self,
        mut fun: F,
    ) -> libmpv2::Result<T> {
        let mut nodes = self
            .fields
            .iter()
            .map(|field| libmpv2_sys::mpv_node {
                u: libmpv2_sys::mpv_node__bindgen_ty_1 {
                    string: field.as_ptr().cast_mut(),
                },
                format: libmpv2::mpv_format::String,
            })
            .collect::<Vec<_>>();
        let mut list = libmpv2_sys::mpv_node_list {
            num: self.count,
            values: nodes.as_mut_ptr(),
            keys: std::ptr::null_mut(),
        };
        let mut node = libmpv2_sys::mpv_node {
            u: libmpv2_sys::mpv_node__bindgen_ty_1 { list: &mut list },
            format: libmpv2::mpv_format::Array,
        };
        fun((&mut node as *mut libmpv2_sys::mpv_node).cast())
    }
}
