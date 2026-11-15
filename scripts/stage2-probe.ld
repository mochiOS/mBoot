ENTRY(_start)
SECTIONS {
    . = 0x40080000;
    .text : { KEEP(*(.text.entry)) *(.text .text.*) }
    .guest : ALIGN(4096) {
        __guest_start = .;
        KEEP(*(.guest.text))
        . = ALIGN(4096);
        __guest_end = .;
    }
    .rodata : ALIGN(4096) { *(.rodata .rodata.*) }
    .data : ALIGN(4096) { *(.data .data.*) }
    .bss (NOLOAD) : ALIGN(65536) {
        __bss_start = .;
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
        . += 65536;
        __stack_top = .;
    }
    /DISCARD/ : { *(.eh_frame*) *(.note*) }
}
