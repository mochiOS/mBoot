// Loader fixture only: no kernel is booted or executed by the host check.
.section .head,"ax"
.global bootstrap
bootstrap:
  nop
  b bootstrap
.section .text,"ax"
.global upper_code
upper_code:
  ret
.section .data,"aw"
.quad 0x1122334455667788
.section .bss,"aw",%nobits
.skip 16
.section .note.GNU-stack,"",%progbits
