CSS_INPUT  := crates/schema-forge-acton/static/css/input.css
CSS_OUTPUT := crates/schema-forge-acton/static/css/admin.css
CSS_CONTENT := 'crates/schema-forge-acton/templates/**/*.html'

.PHONY: css css-watch

css:
	./tailwindcss -i $(CSS_INPUT) -o $(CSS_OUTPUT) --content $(CSS_CONTENT)

css-watch:
	./tailwindcss -i $(CSS_INPUT) -o $(CSS_OUTPUT) --content $(CSS_CONTENT) --watch
