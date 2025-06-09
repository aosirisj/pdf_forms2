# Fill PDF Forms 2
A library to programatically identify and fill out PDF forms.

**WARNING**: This is a fork of "pdf_forms" by [malte-v](https://github.com/malte-v), including changes made by [aosirisj](https://github.com/aosirisj). The original "pdf_form" crate was made by [jsandler18](https://github.com/jsandler18).

## Example Code
Read a PDF and discover the form fields. Add `pdf_forms2 = { path = "path/to/pdf_forms2" }` to your cargo.toml file.
```rust
use pdf_forms2::{Form, FieldType};

// Load the pdf into a form from a path
let form = Form::load("path/to/pdf").unwrap();
// Get all types of the form fields (e.g. Text, Radio, etc) in a Vector
let field_types = form.get_all_types();
// Print the types
for type in field_types {
    println!("{:?}", type);
};

```

Write to the form fields
```rust
use pdf_forms2::{Form, FieldState};

// Load the pdf into a form from a path
let mut form = Form::load("path/to/pdf").unwrap();
form.set_text(0, String::from("filling the field"));
form.save("path/to/new/pdf");

```

## Features added
New functions were added:
- _load 2_ can read inline dictionaries, which translates into fewer restrictions when detecting forms in PDF documents. Additionally, the errors produced by this function are more descriptive, allowing an effective detection of incorrect structures. 
    
```rust
    use pdf_forms2::{Form};

    // Load the pdf into a form from a path
    let form = Form::load2("path/to/pdf").unwrap();
```
- _set\_text\_fs_ and _set\_text\_fs\_ro_ include an additional parameter to adjust the font size of the display appearance. Ensuring that this parameter is not zero helps properly visualize the information entered in the form fields. The second function marks the filled fields as read-only.

```rust
    use pdf_forms2::{Form, FieldState};

    // Load the pdf into a form from a path
    let mut form = Form::load2("path/to/pdf").unwrap();
    form.set_text_fs(0, String::from("filling the field"), 6);
    form.save("path/to/new/pdf");
```