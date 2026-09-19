OUTPUT_ARCH(riscv)
ENTRY(_start)
BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    .text : {
        PROVIDE(_stext = .);
        *(.text.entry)
        *(.text .text.*)
        PROVIDE(_etext = .);
    }
    .rodata : {
        PROVIDE(_srodata = .);
        *(.rodata .rodata.*)
        PROVIDE(_erodata = .);
    }
    .data : {
        PROVIDE(_sdata = .);
        *(.data .data.*)
        *(.sdata .sdata.*)
        PROVIDE(_edata = .);
    }
    .bss : {
        PROVIDE(_sbss = .);
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        PROVIDE(_ebss = .);
    }
    PROVIDE(_ekernel = .);
    . = ALIGN(4K);
    PROVIDE(_heap_start = .);
    /DISCARD/ : {
        *(.eh_frame)
        *(.debug*)
    }
}
