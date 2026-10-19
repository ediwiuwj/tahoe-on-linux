#!/bin/bash
# build.sh: compila desde Linux los programas de macOS de esta carpeta (x86_64, Mach-O) con clang y ld64.lld, sin SDK de
# Apple: las bibliotecas del sistema se describen con stubs .tbd mínimos (solo los símbolos que se usan). Se ejecutan en
# tahoe-run (tools/macos-shell.sh). Uso: tools/macos/build.sh  -> tools/macos/bin/
cd "$(dirname "$(readlink -f "$0")")" || exit 1
mkdir -p bin obj
tbd() {   # tbd <install-name> <símbolos...>
	printf -- '--- !tapi-tbd\ntbd-version: 4\ntargets: [ x86_64-macos ]\ninstall-name: '"'%s'"'\nexports:\n  - targets: [ x86_64-macos ]\n    symbols: [ %s ]\n...\n' "$1" "$(shift; echo "$@" | sed 's/ /, /g')"
}
tbd /usr/lib/libSystem.B.dylib _open _read _write _close _lseek _malloc _free _unlink _printf _strlen _memcpy _snprintf _getline ___stdinp ___stack_chk_fail ___stack_chk_guard dyld_stub_binder > obj/libSystem.tbd
tbd /usr/lib/libcompression.dylib _compression_decode_buffer > obj/libcompression.tbd
for src in *.c; do
	n="${src%.c}"
	clang -target x86_64-apple-macos13 -O2 -ffreestanding -fno-builtin -nostdinc -c "$src" -o "obj/$n.o" || exit 1
	ld64.lld -arch x86_64 -platform_version macos 13.0 26.0 -o "bin/$n" "obj/$n.o" obj/libSystem.tbd obj/libcompression.tbd || exit 1
	echo "bin/$n"
done
