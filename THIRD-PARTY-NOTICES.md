# Third-party notices

Watershed is licensed under the Apache License, Version 2.0 (see `LICENSE`).
Parts of it are derived from other people's work, which is used under the
terms below.

## lifelib

The savings model in `crates/watershed-gpu/examples/shared/savings.rs` and
`crates/watershed-gpu/examples/shared/savings_jax.py` is a reimplementation
of `savings.CashValue_ME_EX2` from lifelib (https://lifelib.io), and
`liabilities_reference.py` and `amendments_reference.py` in the same folder
contain adapted versions of two of its formulas (`lapse_rate` and
`pols_lapse`). lifelib is not distributed with Watershed; those scripts
install and run it. lifelib is used under the MIT License:

```
MIT License

Copyright (c) 2022 lifelib Developers

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
