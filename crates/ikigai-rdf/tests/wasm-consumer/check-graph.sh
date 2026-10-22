#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
for features in '' rdf-12 rdfxml; do
  graph=$(cargo tree --target wasm32-unknown-unknown -e normal --prefix none --features "$features")
  echo "$graph" | grep -q '^rdf-depth-consumer v'
  echo "$graph" | grep -q '^oxttl v'
  if echo "$graph" | grep -E '^(oxrdfio|oxjsonld|oxrdfxml) v'; then
    echo "Scan consumer unexpectedly includes format parsers (features: $features)" >&2
    exit 1
  fi
  if [[ "$features" == rdfxml ]]; then
    echo "$graph" | grep -q '^quick-xml v'
  elif echo "$graph" | grep -E '^quick-xml v'; then
    echo "Turtle consumer unexpectedly includes quick-xml (features: $features)" >&2
    exit 1
  fi
done
