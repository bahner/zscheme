LIBRARIES := stdlib runtime avatar events
LIBRARY_SOURCES := $(LIBRARIES:%=%.zscheme)
LIBRARY_CIDS := $(LIBRARIES:%=%.cid)
BOOTSTRAP := .my.z.scheme
BOOTSTRAP_CID := $(BOOTSTRAP).cid

.PHONY: publish zscheme-cid cids clean

# Publish the individual libraries, then their combined source.
publish: $(LIBRARY_CIDS) $(BOOTSTRAP_CID)

# Publish only the combined Zion startup source.
zscheme-cid: $(BOOTSTRAP_CID)

%.cid: %.zscheme
	@echo "Publishing $<..."
	@ipfs add -q --cid-version 1 "$<" | tee "$@"
	@echo "  -> $< : $$(cat $@)"

$(BOOTSTRAP): $(LIBRARY_SOURCES)
	cat $(LIBRARY_SOURCES) > "$@"

$(BOOTSTRAP_CID): $(BOOTSTRAP)
	@echo "Publishing $<..."
	@ipfs add -q --cid-version 1 "$<" | tee "$@"
	@echo "  -> $< : $$(cat $@)"

cids:
	@for file in $(LIBRARY_CIDS) $(BOOTSTRAP_CID); do \
		name=$$(basename "$$file" .cid); \
		if [ -f "$$file" ]; then \
			printf '%-30s %s\n' "$$name" "$$(cat "$$file")"; \
		else \
			printf '%-30s (not published)\n' "$$name"; \
		fi; \
	done

clean:
	@echo "Nothing to clean (keep .cid files in repo)"
