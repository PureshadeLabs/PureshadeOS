# Build mkrfs2 using rustc directly (same pattern as mkrfs).
# This avoids inheriting the kernel workspace's build-std config.
# The fs/rfs2 crate (the exact code the kernel mounts) is compiled as an
# rlib and linked in — mkrfs2 contains no on-disk format logic of its own.

BIN      := mkrfs2
SRC      := src/main.rs
RFS2_SRC := ../../fs/rfs2/src
RFS2_LIB := librfs2.rlib

.PHONY: all clean

all: $(BIN)

$(RFS2_LIB): $(wildcard $(RFS2_SRC)/*.rs)
	rustc --edition 2021 -O --crate-name rfs2 --crate-type rlib -o $@ $(RFS2_SRC)/lib.rs

$(BIN): $(SRC) $(RFS2_LIB)
	rustc --edition 2021 -O --extern rfs2=$(RFS2_LIB) -o $@ $(SRC)

clean:
	rm -f $(BIN) $(RFS2_LIB)
