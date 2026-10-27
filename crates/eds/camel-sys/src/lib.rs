//! The two libcamel types libedataserver mentions (`e_source_camel_*`). Nyx does
//! not use Camel, and these are only passed by pointer, so they are opaque here.
//! The installed Camel GIR also has entries gir cannot turn into working code.

#[repr(C)]
pub struct CamelService {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}

#[repr(C)]
pub struct CamelSettings {
    _data: [u8; 0],
    _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
}
