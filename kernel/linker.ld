ENTRY(kernel_main)

PHDRS
{
    text    PT_LOAD    FLAGS((1 << 0) | (1 << 2)) ; /* Execute + Read */
    rodata  PT_LOAD    FLAGS((1 << 2)) ;            /* Read only */
    data    PT_LOAD    FLAGS((1 << 1) | (1 << 2)) ; /* Write + Read */
}

SECTIONS
{
    . = 0xffffffff80000000;

    /* Bounds of the whole kernel image, used by paging.rs to know exactly
       how many pages to map without hand-maintaining a size constant. */
    PROVIDE(__kernel_start = .);

    PROVIDE(__text_start = .);
    .text : {
        *(.text .text.*)
    } :text
    PROVIDE(__text_end = .);

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    PROVIDE(__rodata_start = .);
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata
    PROVIDE(__rodata_end = .);

    . = ALIGN(CONSTANT(MAXPAGESIZE));

    PROVIDE(__data_start = .);
    .data : {
        *(.data .data.*)

        /* Limine request structs live here so they land near real data
           and never get stripped from the binary. */
        KEEP(*(.requests_start_marker))
        KEEP(*(.requests))
        KEEP(*(.requests_end_marker))
    } :data

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    PROVIDE(__kernel_end = .);

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
