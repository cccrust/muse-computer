//! Linker-provided symbols (addresses only; never dereferenced).
//! Defined in linker.ld; declared once here and shared via `memory::`.

unsafe extern "C" {
    pub(crate) static _stext: u8;
    pub(crate) static _etext: u8;
    pub(crate) static _srodata: u8;
    pub(crate) static _erodata: u8;
    pub(crate) static _sdata: u8;
    pub(crate) static _edata: u8;
    pub(crate) static _sbss: u8;
    pub(crate) static _ebss: u8;
    pub(crate) static _boot_stack_bottom: u8;
    pub(crate) static _boot_stack_top: u8;
    pub(crate) static _trap_stack_top: u8;
}
