LIBRARIES := stdlib runtime
LIBRARY_SOURCES := $(LIBRARIES:%=%.zscheme)
LIBRARY_CIDS := $(LIBRARIES:%=%.cid)
BOOTSTRAP := .my.scheme
BOOTSTRAP_CID := $(BOOTSTRAP).cid

.PHONY: publish cids clean

# Publish the individual libraries, then a single file that includes both.
publish: $(LIBRARY_CIDS) $(BOOTSTRAP_CID)

%.cid: %.zscheme
	@echo "Publishing $<..."
	@ipfs add -q --cid-version 1 "$<" | tee "$@"
	@echo "  -> $< : $$(cat $@)"

$(BOOTSTRAP): .gen.my.scheme $(LIBRARY_CIDS)
	sh ./.gen.my.scheme > "$@"

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
