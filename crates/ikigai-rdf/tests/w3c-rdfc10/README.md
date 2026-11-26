# W3C RDFC-1.0 test vectors

Copied from the W3C RDF Dataset Canonicalization repository at commit
`5ffaaa13c0e19a2a2f9e60d0ecd9efaf3edab633`:
<https://github.com/w3c/rdf-canon/tree/5ffaaa13c0e19a2a2f9e60d0ecd9efaf3edab633/tests>.
See `LICENCE.md` for the upstream license. The JSON-LD manifest drives the test;
all its canonical output and identifier-map cases are checked, including SHA-384
through the internal implementation, and its negative poison case must be refused.
The public API uses the RDFC-1.0 default SHA-256.
