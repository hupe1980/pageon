# Known html5lib differences

1787 of 1792 cases pass (reference DOM: 1787). Every case below fails
identically in html5ever's reference DOM, so the cause is the parser, not pageon's DOM.

| Case | Cause |
| --- | --- |
| tests_innerHTML_1.dat #76 | reference parser |
| webkit02.dat #45 | reference parser |
| webkit02.dat #46 | reference parser |
| webkit02.dat #47 | reference parser |
| webkit02.dat #48 | reference parser |
