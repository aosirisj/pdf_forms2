#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate derive_error;

mod utils;

use std::collections::VecDeque;
use std::io;
use std::io::Write;
use std::path::Path;
use std::str;

use bitflags::_core::str::from_utf8;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};

use crate::utils::*;

/// A PDF Form that contains fillable fields
///
/// Use this struct to load an existing PDF with a fillable form using the `load` method.  It will
/// analyze the PDF and identify the fields. Then you can get and set the content of the fields by
/// index.
pub struct Form {
    pub document: Document,
    pub form_ids: Vec<ObjectId>,
}

/// The possible types of fillable form fields in a PDF
#[derive(Debug)]
pub enum FieldType {
    Button,
    Radio,
    CheckBox,
    ListBox,
    ComboBox,
    Text,
    Unknown,
}

#[derive(Debug, Error)]
/// Errors that may occur while loading a PDF
pub enum LoadError {
    /// An Lopdf Error
    LopdfError(lopdf::Error),
    /// The reference `ObjectId` did not point to any values
    #[error(non_std, no_from)]
    NoSuchReference(ObjectId),
    /// An element that was expected to be a reference was not a reference
    NotAReference,
    // Add: Error for incorrect structures
    #[error(msg_embedded, non_std, no_from)]
    StructureError(String)
}

/// Errors That may occur while setting values in a form
#[derive(Debug, Error)]
pub enum ValueError {
    /// The method used to set the state is incompatible with the type of the field
    TypeMismatch,
    /// One or more selected values are not valid choices
    InvalidSelection,
    /// Multiple values were selected when only one was allowed
    TooManySelected,
    /// Readonly field cannot be edited
    Readonly,
    /// Field not found
    NotFound,
}

/// The current state of a form field
#[derive(Debug)]
pub enum FieldState {
    /// Push buttons have no state
    Button,
    /// `selected` is the singular option from `options` that is selected
    Radio {
        selected: String,
        options: Vec<String>,
        readonly: bool,
        required: bool,
    },
    /// The toggle state of the checkbox
    CheckBox {
        is_checked: bool,
        readonly: bool,
        required: bool,
    },
    /// `selected` is the list of selected options from `options`
    ListBox {
        selected: Vec<String>,
        options: Vec<String>,
        multiselect: bool,
        readonly: bool,
        required: bool,
    },
    /// `selected` is the list of selected options from `options`
    ComboBox {
        selected: Vec<String>,
        options: Vec<String>,
        editable: bool,
        readonly: bool,
        required: bool,
    },
    /// User Text Input
    Text {
        text: String,
        readonly: bool,
        required: bool,
    },
    /// Unknown fields have no state
    Unknown,
}

trait PdfObjectDeref {
    fn deref<'a>(&self, doc: &'a Document) -> Result<&'a Object, LoadError>;
}

impl PdfObjectDeref for Object {
    fn deref<'a>(&self, doc: &'a Document) -> Result<&'a Object, LoadError> {
        match *self {
            Object::Reference(oid) => doc.objects.get(&oid).ok_or(LoadError::NoSuchReference(oid)),
            _ => Err(LoadError::NotAReference),
        }
    }
}

impl Form {
    /// Takes a reader containing a PDF with a fillable form, analyzes the content, and attempts to
    /// identify all of the fields the form has.
    pub fn load_from<R: io::Read>(reader: R) -> Result<Self, LoadError> {
        let doc = Document::load_from(reader)?;
        Self::load_doc(doc)
    }

    /// Takes a path to a PDF with a fillable form, analyzes the file, and attempts to identify all
    /// of the fields the form has.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, LoadError> {
        let doc = Document::load(path)?;
        Self::load_doc(doc)
    }

    pub fn load2<P: AsRef<Path>>(path: P) -> Result<Form, LoadError> {
        let doc = Document::load(path)?;
        Self::load_doc2(doc)
    }
    // New method for reading documents; it handles inline dictionaries and some unexpected errors.
    // Also aimed to make error messages more descriptive.
    // To use this function, use _load2_ instead of _load_, which uses _load_doc_ from the original _forms_pdf_ crate.
    fn load_doc2(document: Document) -> Result<Self, LoadError> {    
        let mut form_ids = Vec::new();
        let mut queue = VecDeque::new(); 

        {// Block so borrow of doc ends before doc is moved into the result

        // 0. Get root dict
        let root_dict = &document
                            .trailer
                            .get(b"Root")?  
                            .deref(&document)?
                            .as_dict()?;

        // 1. Get an AcroForm object (it could be of different types, see 2.)
        let acroform_obj  = match root_dict.get(b"AcroForm") {
                                Ok(o) => o,
                                Err(_) => return Err(LoadError::StructureError("Key \"AcroForm\" doesn't exist in document".into()))
                            };

        // 2. Get the fields object contained in AcroForm, which can be a reference or an inline dictionary
        let fields_obj = match acroform_obj {
            Object::Reference(obj_id) => {
                let acroform = match document.objects.get(obj_id) {
                    Some(Object::Dictionary(dict)) => dict,
                    Some(_) => return Err(LoadError::StructureError("AcroForm cannot be parsed to a dictionary".into())),
                    None => return Err(LoadError::StructureError("Invalid reference to AcroForm".into())),
                };
                match acroform.get(b"Fields") {
                    Ok(obj) => obj,
                    Err(_) => return Err(LoadError::StructureError("Key \"Fields\" doesn't exist in AcroForm".into())),
                }
            }
            Object::Dictionary(dict) => {
                match dict.get(b"Fields") {
                    Ok(obj) => obj,
                    Err(_) => return Err(LoadError::StructureError("Key \"Fields\" doesn't exist in AcroForm".into())),
                }
            }
            _ => return Err(LoadError::StructureError("AcroForm is not a reference neither a dictionary".into())),
        };

        // 3. Get the fields in an array and transform it into a double-ended queue.
        // Again, the fields object obtained in 2. can be either a reference or a dictionary.
        let fields_array = {
            match fields_obj {
            Object::Array(arr) => arr,
            Object::Reference(obj_id) => {
                let deref_obj = document.get_object(*obj_id)?;
                deref_obj.as_array()?
            },
            _ => return Err(LoadError::NotAReference),
        }};

        queue.extend(fields_array.iter().cloned());

        // 4. Iterate the field queue, from parents to children
        while let Some(objref) = queue.pop_front() {
            let obj = match objref.deref(&document) {
                Ok(o) => o,
                Err(_) => continue, // Skip if the field cannot be dereferenced, maybe other fields can be read
            };

            if let Object::Dictionary(ref dict) = *obj {
                // If the field has a "FT" key, then it receives input and it is added to the list of field IDs (form_ids)
                if dict.get(b"FT").is_ok() {
                    if let Ok(reference) = objref.as_reference() {
                        form_ids.push(reference);
                    }
                }

                // Another option is that the field has children. If that's the case, add them to the queue
                if let Ok(&Object::Array(ref kids)) = dict.get(b"Kids") {
                    queue.extend(kids.iter().cloned());
                }
            }
        }
        }
        
        // 5. Return the original document and the vector with the IDs that store a form field
        Ok(Form {
            document,
            form_ids,
        })
    }

    fn load_doc(mut document: Document) -> Result<Self, LoadError> {
        let mut form_ids = Vec::new();
        let mut queue = VecDeque::new();
        // Block so borrow of doc ends before doc is moved into the result
        {
            let acroform = document
                .objects
                .get_mut(
                    &document
                        .trailer
                        .get(b"Root")?
                        .deref(&document)?
                        .as_dict()?
                        .get(b"AcroForm")?
                        .as_reference()?,
                )
                .ok_or(LoadError::NotAReference)?
                .as_dict_mut()?;

            let fields_list = acroform.get(b"Fields")?.as_array()?;
            queue.append(&mut VecDeque::from(fields_list.clone()));

            // Iterate over the fields
            while let Some(objref) = queue.pop_front() {
                let obj = objref.deref(&document)?;
                if let Object::Dictionary(ref dict) = *obj {
                    // If the field has FT, it actually takes input.  Save this
                    if dict.get(b"FT").is_ok() {
                        form_ids.push(objref.as_reference().unwrap());
                    }

                    // If this field has kids, they might have FT, so add them to the queue
                    if let Ok(&Object::Array(ref kids)) = dict.get(b"Kids") {
                        queue.append(&mut VecDeque::from(kids.clone()));
                    }
                }
            }
        }
        Ok(Form { document, form_ids })
    }

    /// Returns the number of fields the form has
    pub fn len(&self) -> usize {
        self.form_ids.len()
    }

    /// Returns true if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Gets the type of field of the given index
    ///
    /// # Panics
    /// This function will panic if the index is greater than the number of fields
    pub fn get_type(&self, n: usize) -> FieldType {
        // unwraps should be fine because load should have verified everything exists
        let field = self
            .document
            .objects
            .get(&self.form_ids[n])
            .unwrap()
            .as_dict()
            .unwrap();

        let type_str = field.get(b"FT").unwrap().as_name_str().unwrap();
        if type_str == "Btn" {
            let flags = ButtonFlags::from_bits_truncate(get_field_flags(field));
            if flags.intersects(ButtonFlags::RADIO | ButtonFlags::NO_TOGGLE_TO_OFF) {
                FieldType::Radio
            } else if flags.intersects(ButtonFlags::PUSHBUTTON) {
                FieldType::Button
            } else {
                FieldType::CheckBox
            }
        } else if type_str == "Ch" {
            let flags = ChoiceFlags::from_bits_truncate(get_field_flags(field));
            if flags.intersects(ChoiceFlags::COBMO) {
                FieldType::ComboBox
            } else {
                FieldType::ListBox
            }
        } else if type_str == "Tx" {
            FieldType::Text
        } else {
            FieldType::Unknown
        }
    }

    /// Gets the name of field of the given index
    ///
    /// # Panics
    /// This function will panic if the index is greater than the number of fields
    pub fn get_name(&self, n: usize) -> Option<String> {
        // unwraps should be fine because load should have verified everything exists
        let field = self
            .document
            .objects
            .get(&self.form_ids[n])
            .unwrap()
            .as_dict()
            .unwrap();

        // The "T" key refers to the name of the field
        match field.get(b"T") {
            Ok(Object::String(data, _)) => String::from_utf8(data.clone()).ok(),
            _ => None,
        }
    }

    /// Gets the types of all of the fields in the form
    pub fn get_all_types(&self) -> Vec<FieldType> {
        let mut res = Vec::with_capacity(self.len());
        for i in 0..self.len() {
            res.push(self.get_type(i))
        }
        res
    }

    /// Gets the names of all of the fields in the form
    pub fn get_all_names(&self) -> Vec<Option<String>> {
        let mut res = Vec::with_capacity(self.len());
        for i in 0..self.len() {
            res.push(self.get_name(i))
        }
        res
    }

    /// Gets the state of field of the given index
    ///
    /// # Panics
    /// This function will panic if the index is greater than the number of fields
    pub fn get_state(&self, n: usize) -> FieldState {
        let field = self
            .document
            .objects
            .get(&self.form_ids[n])
            .unwrap()
            .as_dict()
            .unwrap();
        match self.get_type(n) {
            FieldType::Button => FieldState::Button,
            FieldType::Radio => FieldState::Radio {
                selected: match field.get(b"V") {
                    Ok(name) => name.as_name_str().unwrap().to_owned(),
                    _ => match field.get(b"AS") {
                        Ok(name) => name.as_name_str().unwrap().to_owned(),
                        _ => "".to_owned(),
                    },
                },
                options: self.get_possibilities(self.form_ids[n]),
                readonly: is_read_only(field),
                required: is_required(field),
            },
            FieldType::CheckBox => FieldState::CheckBox {
                is_checked: match field.get(b"V") {
                    Ok(name) => name.as_name_str().unwrap() == "Yes",
                    _ => match field.get(b"AS") {
                        Ok(name) => name.as_name_str().unwrap() == "Yes",
                        _ => false,
                    },
                },
                readonly: is_read_only(field),
                required: is_required(field),
            },
            FieldType::ListBox => FieldState::ListBox {
                // V field in a list box can be either text for one option, an array for many
                // options, or null
                selected: match field.get(b"V") {
                    Ok(selection) => match *selection {
                        Object::String(ref s, StringFormat::Literal) => {
                            vec![str::from_utf8(&s).unwrap().to_owned()]
                        }
                        Object::Array(ref chosen) => {
                            let mut res = Vec::new();
                            for obj in chosen {
                                if let Object::String(ref s, StringFormat::Literal) = *obj {
                                    res.push(str::from_utf8(&s).unwrap().to_owned());
                                }
                            }
                            res
                        }
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                },
                // The options is an array of either text elements or arrays where the second
                // element is what we want
                options: match field.get(b"Opt") {
                    Ok(&Object::Array(ref options)) => options
                        .iter()
                        .map(|x| match *x {
                            Object::String(ref s, StringFormat::Literal) => {
                                str::from_utf8(&s).unwrap().to_owned()
                            }
                            Object::Array(ref arr) => {
                                if let Object::String(ref s, StringFormat::Literal) = &arr[1] {
                                    str::from_utf8(&s).unwrap().to_owned()
                                } else {
                                    String::new()
                                }
                            }
                            _ => String::new(),
                        })
                        .filter(|x| !x.is_empty())
                        .collect(),
                    _ => Vec::new(),
                },
                multiselect: {
                    let flags = ChoiceFlags::from_bits_truncate(get_field_flags(field));
                    flags.intersects(ChoiceFlags::MULTISELECT)
                },
                readonly: is_read_only(field),
                required: is_required(field),
            },
            FieldType::ComboBox => FieldState::ComboBox {
                // V field in a list box can be either text for one option, an array for many
                // options, or null
                selected: match field.get(b"V") {
                    Ok(selection) => match *selection {
                        Object::String(ref s, StringFormat::Literal) => {
                            vec![str::from_utf8(&s).unwrap().to_owned()]
                        }
                        Object::Array(ref chosen) => {
                            let mut res = Vec::new();
                            for obj in chosen {
                                if let Object::String(ref s, StringFormat::Literal) = *obj {
                                    res.push(str::from_utf8(&s).unwrap().to_owned());
                                }
                            }
                            res
                        }
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                },
                // The options is an array of either text elements or arrays where the second
                // element is what we want
                options: match field.get(b"Opt") {
                    Ok(&Object::Array(ref options)) => options
                        .iter()
                        .map(|x| match *x {
                            Object::String(ref s, StringFormat::Literal) => {
                                str::from_utf8(&s).unwrap().to_owned()
                            }
                            Object::Array(ref arr) => {
                                if let Object::String(ref s, StringFormat::Literal) = &arr[1] {
                                    str::from_utf8(&s).unwrap().to_owned()
                                } else {
                                    String::new()
                                }
                            }
                            _ => String::new(),
                        })
                        .filter(|x| !x.is_empty())
                        .collect(),
                    _ => Vec::new(),
                },
                editable: {
                    let flags = ChoiceFlags::from_bits_truncate(get_field_flags(field));

                    flags.intersects(ChoiceFlags::EDIT)
                },
                readonly: is_read_only(field),
                required: is_required(field),
            },
            FieldType::Text => FieldState::Text {
                text: match field.get(b"V") {
                    Ok(&Object::String(ref s, StringFormat::Literal)) => {
                        str::from_utf8(&s.clone()).unwrap().to_owned()
                    }
                    _ => "".to_owned(),
                },
                readonly: is_read_only(field),
                required: is_required(field),
            },
            FieldType::Unknown => FieldState::Unknown,
        }
    }

    /// Gets the object of field of the given index
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn get_object_id(&self, n: usize) -> ObjectId {
        self.form_ids[n]
    }

    /// If the field at index `n` is a text field, fills in that field with the text `s`.
    /// If it is not a text field, returns ValueError
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn set_text(&mut self, n: usize, s: String) -> Result<(), ValueError> {
        match self.get_state(n) {
            FieldState::Text { .. } => {
                let field = self
                    .document
                    .objects
                    .get_mut(&self.form_ids[n])
                    .unwrap()
                    .as_dict_mut()
                    .unwrap();

                field.set("V", Object::string_literal(s.into_bytes()));

                // Regenerate text appearance confoming the new text but ignore the result
                let _ = self.regenerate_text_appearance(n);

                Ok(())
            }
            _ => Err(ValueError::TypeMismatch),
        }
    }

    // New function to write text that uses the extended function _regenerate_text_appearance2_
    pub fn set_text_fs(&mut self, n: usize, s: String, f:i32) -> Result<(), ValueError> {
        if let FieldState::Text { .. } = self.get_state(n) {
            let field = self
                .document
                .objects
                .get_mut(&self.form_ids[n])
                .unwrap()
                .as_dict_mut()
                .unwrap();

            field.set("V", Object::string_literal(s.into_bytes()));

            // Regenerate the text appearance using the new function. Issues a warning in case
            // it was not regenerated correctly
            if let Err(e) = self.regenerate_text_appearance2(n, f) {
                println!("Text apperance regeneration failed: {e}"); 
            }

            Ok(())
        } else { Err(ValueError::TypeMismatch) }
    }

    // New function to write text that uses the extended function _regenerate_text_appearance2_
    // Additionally, this function marks the filled PDF fields as read-only
    pub fn set_text_fs_ro(&mut self, n: usize, s: String, f:i32) -> Result<(), ValueError> {
        if let FieldState::Text { .. } = self.get_state(n) {
            let field = self
                .document
                .objects
                .get_mut(&self.form_ids[n])
                .unwrap()
                .as_dict_mut()
                .unwrap();

            field.set("V", Object::string_literal(s.into_bytes()));

            //This block sets the read-only flag (bit 0 of Ff)            
            let mut v = 0;
            match field.get(b"Ff") {
                Ok(f) => {
                    if let Object::Integer(val) = f {
                    v = *val;
                    }
                }
                Err(_) => { v = 0; }
            }
            let new_flags = v | 1 << 0;
            field.set(b"Ff", new_flags);

            // Regenerate the text appearance using the new function. Issues a warning in case
            // it was not regenerated correctly
            if let Err(e) = self.regenerate_text_appearance2(n, f) {
                println!("Text apperance regeneration failed: {e}"); 
            }

            Ok(())
        } else { Err(ValueError::TypeMismatch) }
    }

    /// Regenerates the appearance for the field at index `n` due to an alteration of the
    /// original TextField value, the AP will be updated accordingly.
    ///
    /// # Incomplete
    /// This function is not exhaustive as not parse the original TextField orientation
    /// or the text alignment and other kind of enrichments, also doesn't discover for
    /// the global document DA.
    ///
    /// A more sophisticated parser is needed here
    fn regenerate_text_appearance(&mut self, n: usize) -> Result<(), lopdf::Error> {
        let field = {
            self.document
                .objects
                .get(&self.form_ids[n])
                .unwrap()
                .as_dict()
                .unwrap()
        };

        // The value of the object (should be a string)
        let value = field.get(b"V")?.to_owned();

        // The default appearance of the object (should be a string)
        let da = field.get(b"DA")?.to_owned();

        // The default appearance of the object (should be a string)
        let rect = field
            .get(b"Rect")?
            .as_array()?
            .iter()
            .map(|object| {
                object
                    .as_f64()
                    .unwrap_or(object.as_i64().unwrap_or(0) as f64) as f32
            })
            .collect::<Vec<_>>();

        // Gets the object stream
        let object_id = field.get(b"AP")?.as_dict()?.get(b"N")?.as_reference()?;
        let stream = self.document.get_object_mut(object_id)?.as_stream_mut()?;

        // Decode and get the content, even if is compressed
        let mut content = {
            if let Ok(content) = stream.decompressed_content() {
                Content::decode(&content)?
            } else {
                Content::decode(&stream.content)?
            }
        };

        // Ignored operators
        let ignored_operators = vec![
            "bt", "tc", "tw", "tz", "g", "tm", "tr", "tf", "tj", "et", "q", "bmc", "emc",
        ];

        // Remove these ignored operators as we have to generate the text and fonts again
        content.operations.retain(|operation| {
            !ignored_operators.contains(&operation.operator.to_lowercase().as_str())
        });

        // Let's construct the text widget
        content.operations.append(&mut vec![
            Operation::new("BMC", vec!["Tx".into()]),
            Operation::new("q", vec![]),
            Operation::new("BT", vec![]),
        ]);

        let font = parse_font(match da {
            Object::String(ref bytes, _) => Some(from_utf8(bytes)?),
            _ => None,
        });

        // Define some helping font variables
        let font_name = (font.0).0;
        let font_size = (font.0).1;
        let font_color = font.1;

        // Set the font type and size and color
        content.operations.append(&mut vec![
            Operation::new("Tf", vec![font_name.into(), font_size.into()]),
            Operation::new(
                font_color.0,
                match font_color.0 {
                    "k" => vec![
                        font_color.1.into(),
                        font_color.2.into(),
                        font_color.3.into(),
                        font_color.4.into(),
                    ],
                    "rg" => vec![
                        font_color.1.into(),
                        font_color.2.into(),
                        font_color.3.into(),
                    ],
                    _ => vec![font_color.1.into()],
                },
            ),
        ]);

        // Calculate the text offset
        let x = 2.0; // Suppose this fixed offset as we should have known the border here

        // Formula picked up from Poppler
        let dy = rect[1] - rect[3];
        let y = if dy > 0.0 {
            0.5 * dy - 0.4 * font_size as f32
        } else {
            0.5 * font_size as f32
        };

        // Set the text bounds, first are fixed at "1 0 0 1" and then the calculated x,y
        content.operations.append(&mut vec![Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
        )]);

        // Set the text value and some finalizing operations
        content.operations.append(&mut vec![
            Operation::new("Tj", vec![value]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("EMC", vec![]),
        ]);

        // Set the new content to the original stream and compress it
        if let Ok(encoded_content) = content.encode() {
            stream.set_plain_content(encoded_content);
            let _ = stream.compress();
        }

        Ok(())
    }
    
    // Extended function to regenerate the appearance. Additionally, it takes an i32 argument
    // that serves as the font size for the text of unselected fields (represented
    // in the stream contained in the object with key AP-N). Ensuring this integer is not zero
    // makes the new values of the fields visible when opening the PDF.
    fn regenerate_text_appearance2(&mut self, n: usize, f: i32) -> Result<(), lopdf::Error> {
        let field = {
            self.document
                .objects
                .get(&self.form_ids[n])
                .unwrap()
                .as_dict()
                .unwrap().clone()
        };

        // The value of the object (should be a string)
        let value = field.get(b"V")?.to_owned();

        // The default appearance of the object (should be a string)
        let da_default = concat!("/Helv {f} Tf 0 g").as_bytes().to_vec();
        let da = match field.get(b"DA") {
            Ok(Object::String(bytes, _)) => {
                let s = std::str::from_utf8(bytes).unwrap_or("");

                if s.contains("0 Tf") || s.trim().is_empty() {
                    Object::string_literal(da_default)
                } else {
                    Object::String(bytes.clone(), StringFormat::Literal)
                }
            }
            _ => Object::string_literal(da_default)
        };

        // The default appearance of the object (should be a string)
        let rect = field
            .get(b"Rect")?
            .as_array()?
            .iter()
            .map(|object| {
                object
                    .as_f64()
                    .unwrap_or(object.as_i64().unwrap_or(0) as f64) as f32
            })
            .collect::<Vec<_>>();

        // Gets the object stream
        // Fix: This block was made more robust to allow the AP key
        // to be absent and assign a new one with a default value
        let object_id = match field.get(b"AP") {
            Ok(Object::Dictionary(ap_dict)) => {
                Some(ap_dict.get(b"N").and_then(|n| n.as_reference())?)
            }
            _ => None,
        };

        let object_id = match object_id {
            Some(id) => id,
            None => {
                // New empty stream for AP
                use lopdf::{Stream};

                let stream = Stream::new(Dictionary::new(), Vec::new());
                let new_id = self.document.new_object_id();
                self.document.objects.insert(new_id, Object::Stream(stream));

                let field_mut = self.document
                .objects
                .get_mut(&self.form_ids[n])
                .unwrap()
                .as_dict_mut()
                .unwrap();

                // AP dict with N key to new stream
                let mut ap_dict = Dictionary::new();
                ap_dict.set("N", Object::Reference(new_id));
                field_mut.set("AP", Object::Dictionary(ap_dict));

                new_id
            }
        };

        let stream = self.document.get_object_mut(object_id)?.as_stream_mut()?;

        // Decode and get the content, even if is compressed
        let mut content = {
            if let Ok(content) = stream.decompressed_content() {
                Content::decode(&content)?
            } else {
                Content::decode(&stream.content)?
            }
        };

        // Ignored operators
        let ignored_operators = vec![
            "bt", "tc", "tw", "tz", "g", "tm", "tr", "tf", "tj", "et", "q", "bmc", "emc",
        ];

        // Remove these ignored operators as we have to generate the text and fonts again
        content.operations.retain(|operation| {
            !ignored_operators.contains(&operation.operator.to_lowercase().as_str())
        });

        // Let's construct the text widget
        content.operations.append(&mut vec![
            Operation::new("BMC", vec!["Tx".into()]),
            Operation::new("q", vec![]),
            Operation::new("BT", vec![]),
        ]);

        // This block and the next were modified to parse the DA
        // (either the one found in the document or the default assigned).
        // If the font size is 0, it is replaced by the function argument _f_
        let font = parse_font(match da {
            Object::String(ref bytes, _) => Some(from_utf8(bytes)?),  //Parsear esto mejor para encontrar una manera de capturar el tamaño de fuente
            _ => Some("((\"0\", 0), (\"g\", 0, 0, 0))")
        });

        // Define some helping font variables
        let font_name = (font.0).0;
        let font_size_da = (font.0).1;
        let font_size = if let 0 = font_size_da { f } else { font_size_da };
        let font_color = font.1;

        // Set the font type and size and color
        content.operations.append(&mut vec![
            Operation::new("Tf", vec![font_name.into(), font_size.into()]),
            Operation::new(
                font_color.0,
                match font_color.0 {
                    "k" => vec![
                        font_color.1.into(),
                        font_color.2.into(),
                        font_color.3.into(),
                        font_color.4.into(),
                    ],
                    "rg" => vec![
                        font_color.1.into(),
                        font_color.2.into(),
                        font_color.3.into(),
                    ],
                    _ => vec![font_color.1.into()],
                },
            ),
        ]);

        // Calculate the text offset
        let x = 2.0; // Suppose this fixed offset as we should have known the border here

        // Formula picked up from Poppler
        let dy = rect[1] - rect[3];
        let y = if dy > 0.0 {
            0.5 * dy - 0.4 * font_size as f32
        } else {
            0.5 * font_size as f32
        };

        // Set the text bounds, first are fixed at "1 0 0 1" and then the calculated x,y
        content.operations.append(&mut vec![Operation::new(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
        )]);

        // Set the text value and some finalizing operations
        content.operations.append(&mut vec![
            Operation::new("Tj", vec![value]),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("EMC", vec![]),
        ]);

        // Set the new content to the original stream and compress it
        if let Ok(encoded_content) = content.encode() {
            stream.set_plain_content(encoded_content);
            let _ = stream.compress();
        }

        //self.document.objects.insert(self.form_ids[n], Object::Dictionary(field));
        Ok(())
    }

    /// If the field at index `n` is a checkbox field, toggles the check box based on the value
    /// `is_checked`.
    /// If it is not a checkbox field, returns ValueError
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn set_check_box(&mut self, n: usize, is_checked: bool) -> Result<(), ValueError> {
        match self.get_state(n) {
            FieldState::CheckBox { .. } => {
                let field = self
                    .document
                    .objects
                    .get_mut(&self.form_ids[n])
                    .unwrap()
                    .as_dict_mut()
                    .unwrap();

                let on = get_on_value(field);
                let state = Object::Name(
                    if is_checked { on.as_str() } else { "Off" }
                        .to_owned()
                        .into_bytes(),
                );

                field.set("V", state.clone());
                field.set("AS", state);

                Ok(())
            }
            _ => Err(ValueError::TypeMismatch),
        }
    }

    /// If the field at index `n` is a radio field, toggles the radio button based on the value
    /// `choice`
    /// If it is not a radio button field or the choice is not a valid option, returns ValueError
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn set_radio(&mut self, n: usize, choice: String) -> Result<(), ValueError> {
        match self.get_state(n) {
            FieldState::Radio { options, .. } => {
                if options.contains(&choice) {
                    let field = self
                        .document
                        .objects
                        .get_mut(&self.form_ids[n])
                        .unwrap()
                        .as_dict_mut()
                        .unwrap();
                    field.set("V", Object::Name(choice.into_bytes()));
                    Ok(())
                } else {
                    Err(ValueError::InvalidSelection)
                }
            }
            _ => Err(ValueError::TypeMismatch),
        }
    }

    /// If the field at index `n` is a listbox field, selects the options in `choice`
    /// If it is not a listbox field or one of the choices is not a valid option, or if too many choices are selected, returns ValueError
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn set_list_box(&mut self, n: usize, choices: Vec<String>) -> Result<(), ValueError> {
        match self.get_state(n) {
            FieldState::ListBox {
                options,
                multiselect,
                ..
            } => {
                if choices.iter().fold(true, |a, h| options.contains(h) && a) {
                    if !multiselect && choices.len() > 1 {
                        Err(ValueError::TooManySelected)
                    } else {
                        let field = self
                            .document
                            .objects
                            .get_mut(&self.form_ids[n])
                            .unwrap()
                            .as_dict_mut()
                            .unwrap();
                        match choices.len() {
                            0 => field.set("V", Object::Null),
                            1 => field.set(
                                "V",
                                Object::String(
                                    choices[0].clone().into_bytes(),
                                    StringFormat::Literal,
                                ),
                            ),
                            _ => field.set(
                                "V",
                                Object::Array(
                                    choices
                                        .iter()
                                        .map(|x| {
                                            Object::String(
                                                x.clone().into_bytes(),
                                                StringFormat::Literal,
                                            )
                                        })
                                        .collect(),
                                ),
                            ),
                        };
                        Ok(())
                    }
                } else {
                    Err(ValueError::InvalidSelection)
                }
            }
            _ => Err(ValueError::TypeMismatch),
        }
    }

    /// If the field at index `n` is a combobox field, selects the options in `choice`
    /// If it is not a combobox field or one of the choices is not a valid option, or if too many choices are selected, returns ValueError
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn set_combo_box(&mut self, n: usize, choice: String) -> Result<(), ValueError> {
        match self.get_state(n) {
            FieldState::ComboBox {
                options, editable, ..
            } => {
                if options.contains(&choice) || editable {
                    let field = self
                        .document
                        .objects
                        .get_mut(&self.form_ids[n])
                        .unwrap()
                        .as_dict_mut()
                        .unwrap();
                    field.set(
                        "V",
                        Object::String(choice.into_bytes(), StringFormat::Literal),
                    );
                    Ok(())
                } else {
                    Err(ValueError::InvalidSelection)
                }
            }
            _ => Err(ValueError::TypeMismatch),
        }
    }

    /// Removes the field at index `n`
    ///
    /// # Panics
    /// Will panic if n is larger than the number of fields
    pub fn remove_field(&mut self, n: usize) -> Result<(), ValueError> {
        self.document
            .remove_object(&self.get_object_id(n))
            .map_err(|_| ValueError::NotFound)
    }

    /// Saves the form to the specified path
    pub fn save<P: AsRef<Path>>(&mut self, path: P) -> Result<(), io::Error> {
        self.document.save(path).map(|_| ())
    }

    /// Saves the form to the specified path
    pub fn save_to<W: Write>(&mut self, target: &mut W) -> Result<(), io::Error> {
        self.document.save_to(target)
    }

    fn get_possibilities(&self, oid: ObjectId) -> Vec<String> {
        let mut res = Vec::new();
        let kids_obj = self
            .document
            .objects
            .get(&oid)
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Kids");
        if let Ok(&Object::Array(ref kids)) = kids_obj {
            for (i, kid) in kids.iter().enumerate() {
                let mut found = false;
                if let Ok(&Object::Dictionary(ref appearance_states)) = kid
                    .deref(&self.document)
                    .unwrap()
                    .as_dict()
                    .unwrap()
                    .get(b"AP")
                {
                    if let Ok(&Object::Dictionary(ref normal_appearance)) =
                        appearance_states.get(b"N")
                    {
                        for (key, _) in normal_appearance {
                            if key != b"Off" {
                                res.push(from_utf8(key).unwrap_or("").to_owned());
                                found = true;
                                break;
                            }
                        }
                    }
                }

                if !found {
                    res.push(i.to_string());
                }
            }
        }

        res
    }
}
