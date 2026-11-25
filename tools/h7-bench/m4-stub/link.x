/* Cortex-M4 of the STM32H755: its default boot address is flash bank 2,
   0x0810_0000 (RM0399 Rev 5 section 2.6, Table 9). The image is a vector
   table and a few instructions; it uses no RAM at all. */
MEMORY {
  FLASH : ORIGIN = 0x08100000, LENGTH = 1024K
}
ENTRY(Reset)
SECTIONS {
  .vectors ORIGIN(FLASH) : { KEEP(*(.vectors)) } > FLASH
  .text : { *(.text .text.*) } > FLASH
  /DISCARD/ : { *(.ARM.exidx .ARM.exidx.* .ARM.extab .ARM.extab.*) }
}
ASSERT(SIZEOF(.vectors) == 0x40, "vector table must be 16 words")
