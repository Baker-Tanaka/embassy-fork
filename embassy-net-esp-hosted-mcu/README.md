# ESP-Hosted MCU `embassy-net` integration

[`embassy-net`](https://crates.io/crates/embassy-net) integration for Espressif SoCs running the [ESP-Hosted MCU](https://github.com/espressif/esp-hosted-mcu) stack.

## Interoperability

This crate can run on any executor.

It supports any SPI driver implementing [`embedded-hal-async`](https://crates.io/crates/embedded-hal-async).

## SPI startup diagnostics

With logging enabled, `SpiInterface` reports HS/DR input levels and the completed
transfer count every five seconds while waiting for either signal. The first
four transfers log their start and the received transport header (at debug
level); payloads, including Wi-Fi credentials, are not logged by these
diagnostics. Pin waits retain the original active-high behavior.

These diagnostics distinguish a missing handshake, a missing data-ready signal,
an unfinished SPI transfer, and a received but invalid frame. A DHCP DISCOVER
log with an all-zero MAC is not proof that the SPI link works: the control
initialization must receive ESPInit and obtain the slave MAC first.

The ESPInit RPC identifier and basic transport header were compared against
ESP-Hosted MCU 2.12.9 ([release source](https://github.com/espressif/esp-hosted-mcu/tree/048c92ba1328a8d9b1acc53ac03eaf426d8d0f7f)).
This is not a claim of full protocol or hardware compatibility.

The generated `Rpc` decoder has a large oneof. On small MCUs, optimize the
application crate as well as dependencies: generic decoders can be
monomorphized in the application with its optimization level. In an RP2040
debug build at application `opt-level = 0`, the decoder allocated 612,708 stack
bytes, exceeding the entire 264 KiB RAM and producing `VarIntLimit` even for a
valid 10-byte ESPInit event. A package-only dependency optimization override
does not prevent this. Keep debug symbols and debug assertions enabled, but
use an optimized application build and inspect the resulting stack allocation.
With application `opt-level = 2` in the same build, the decoder's local stack
allocation dropped to 3,036 bytes (plus 20 bytes of saved registers); this is
not a bound on the entire async task's stack.

RPC logs contain identifiers and lengths, not request/response payloads, because
Wi-Fi configuration RPCs carry credentials. Transport traces are header-only.
