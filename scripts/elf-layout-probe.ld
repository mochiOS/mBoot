ENTRY(bootstrap)
PHDRS { head PT_LOAD FLAGS(5); kernel PT_LOAD FLAGS(5); data PT_LOAD FLAGS(6); }
SECTIONS {
  .head 0x10000 : { *(.head) } :head
  .text 0xffff800000000000 : AT(0x11000) { *(.text) } :kernel
  .data ALIGN(4096) : AT(0x12000) { *(.data) } :data
  .bss : AT(0x12008) { *(.bss) } :data
  /DISCARD/ : { *(.comment) *(.note*) }
}
