OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80000000;

SECTIONS
{
  . = BASE_ADDRESS;

  .text : ALIGN(4)
  {
    PROVIDE(_stext = .);
    *(.text.init)
    *(.text .text.*)
    PROVIDE(_etext = .);
  }

  .rodata : ALIGN(4)
  {
    PROVIDE(_srodata = .);
    *(.rodata .rodata.*)
    PROVIDE(_erodata = .);
  }

  .data : ALIGN(4)
  {
    PROVIDE(_sdata = .);
    *(.sdata .sdata.*)
    *(.data .data.*)
    PROVIDE(_edata = .);
  }

  .bss : ALIGN(4)
  {
    PROVIDE(_sbss = .);
    *(.sbss .sbss.*)
    *(.bss .bss.*)
    *(COMMON)
    . = ALIGN(4);
    PROVIDE(_ebss = .);
  }

  /* boot-time temporary stack (used before scheduler starts) */
  . = ALIGN(16);
  PROVIDE(_boot_stack_bottom = .);
  . += 8K;
  PROVIDE(_boot_stack_top = .);

  /* dedicated trap stack: rust_trap() runs here, NEVER on a TrapFrame.
     Without this, trap-time Rust stack usage would grow down from the
     current TrapFrame and corrupt neighbouring .data/.rodata. */
  . = ALIGN(16);
  PROVIDE(_trap_stack_bottom = .);
  . += 8K;
  PROVIDE(_trap_stack_top = .);

  PROVIDE(_heap_start = .);

  /DISCARD/ :
  {
    *(.eh_frame)
    *(.debug*)
    *(.comment)
    *(.riscv.attributes)
  }
}
