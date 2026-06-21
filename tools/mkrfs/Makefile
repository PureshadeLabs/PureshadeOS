# Build mkrfs using rustc directly.
# This avoids inheriting the kernel workspace's build-std config.

BIN := mkrfs
SRC := src/main.rs

.PHONY: all clean

all: $(BIN)

$(BIN): $(SRC)
	rustc --edition 2021 -o $@ $<

clean:
	rm -f $(BIN)
