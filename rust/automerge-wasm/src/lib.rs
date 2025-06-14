#![doc(
    html_logo_url = "https://raw.githubusercontent.com/automerge/automerge/main/img/brandmark.svg",
    html_favicon_url = "https:///raw.githubusercontent.com/automerge/automerge/main/img/favicon.ico"
)]
#![warn(
    missing_debug_implementations,
    // missing_docs, // TODO: add documentation!
    rust_2021_compatibility,
    rust_2018_idioms,
    unreachable_pub,
    bad_style,
    dead_code,
    improper_ctypes,
    non_shorthand_field_patterns,
    no_mangle_generic_items,
    overflowing_literals,
    path_statements,
    patterns_in_fns_without_body,
    unconditional_recursion,
    unused,
    unused_allocation,
    unused_comparisons,
    unused_parens,
    while_true
)]
use am::marks::Mark;
use am::transaction::CommitOptions;
use am::transaction::Transactable;
use am::CursorPosition;
use am::OnPartialLoad;
use am::ScalarValue;
use am::StringMigration;
use am::VerificationMode;
use automerge as am;
use automerge::patches::TextRepresentation;
use automerge::TextEncoding;
use automerge::{sync::SyncDoc, AutoCommit, Change, Prop, ReadDoc, Value, ROOT};
use js_sys::{Array, Function, Object, Uint8Array};
use serde::ser::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::TryInto;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

mod export_cache;
mod interop;
mod sync;
mod value;

use interop::{alloc, get_heads, import_obj, js_get, js_set, to_js_err, to_prop, AR, JS};
use sync::SyncState;
use value::Datatype;

use crate::interop::SubValIter;

#[allow(unused_macros)]
macro_rules! log {
    ( $( $t:tt )* ) => {
          web_sys::console::log_1(&format!( $( $t )* ).into());
    };
}

#[wasm_bindgen]
#[derive(Debug)]
pub struct Automerge {
    doc: AutoCommit,
    freeze: bool,
    external_types: HashMap<Datatype, interop::ExternalTypeConstructor>,
}

#[wasm_bindgen]
impl Automerge {
    pub fn new(actor: Option<String>) -> Result<Automerge, error::BadActorId> {
        let mut doc = AutoCommit::default()
            .with_text_rep(TextRepresentation::String(TextEncoding::Utf16CodeUnit));
        if let Some(a) = actor {
            let a = automerge::ActorId::from(hex::decode(a)?.to_vec());
            doc.set_actor(a);
        }
        Ok(Automerge {
            doc,
            freeze: false,
            external_types: HashMap::default(),
        })
    }

    #[allow(clippy::should_implement_trait)]
    pub fn clone(&mut self, actor: Option<String>) -> Result<Automerge, error::BadActorId> {
        let mut automerge = Automerge {
            doc: self.doc.clone(),
            freeze: self.freeze,
            external_types: self.external_types.clone(),
        };
        if let Some(s) = actor {
            let actor = automerge::ActorId::from(hex::decode(s)?.to_vec());
            automerge.doc.set_actor(actor);
        }
        Ok(automerge)
    }

    pub fn fork(
        &mut self,
        actor: Option<String>,
        heads: JsValue,
    ) -> Result<Automerge, error::Fork> {
        let heads: Result<Vec<am::ChangeHash>, _> = JS(heads).try_into();
        let doc = if let Ok(heads) = heads {
            self.doc.fork_at(&heads)?
        } else {
            self.doc.fork()
        };
        let mut automerge = Automerge {
            doc,
            freeze: self.freeze,
            external_types: self.external_types.clone(),
        };
        if let Some(s) = actor {
            let actor =
                automerge::ActorId::from(hex::decode(s).map_err(error::BadActorId::from)?.to_vec());
            automerge.doc.set_actor(actor);
        }
        Ok(automerge)
    }

    #[wasm_bindgen(js_name = pendingOps)]
    pub fn pending_ops(&self) -> JsValue {
        (self.doc.pending_ops() as u32).into()
    }

    pub fn commit(&mut self, message: Option<String>, time: Option<f64>) -> JsValue {
        let mut commit_opts = CommitOptions::default();
        if let Some(message) = message {
            commit_opts.set_message(message);
        }
        if let Some(time) = time {
            commit_opts.set_time(time as i64);
        }
        let hash = self.doc.commit_with(commit_opts);
        match hash {
            Some(h) => JsValue::from_str(&hex::encode(h.0)),
            None => JsValue::NULL,
        }
    }

    pub fn merge(&mut self, other: &mut Automerge) -> Result<Array, error::Merge> {
        let heads = self.doc.merge(&mut other.doc)?;
        let heads: Array = heads
            .iter()
            .map(|h| JsValue::from_str(&hex::encode(h.0)))
            .collect();
        Ok(heads)
    }

    pub fn rollback(&mut self) -> f64 {
        self.doc.rollback() as f64
    }

    pub fn keys(&self, obj: JsValue, heads: Option<Array>) -> Result<Array, error::Get> {
        let (obj, _) = self.import(obj)?;
        let result = if let Some(heads) = get_heads(heads)? {
            self.doc
                .keys_at(&obj, &heads)
                .map(|s| JsValue::from_str(&s))
                .collect()
        } else {
            self.doc.keys(&obj).map(|s| JsValue::from_str(&s)).collect()
        };
        Ok(result)
    }

    pub fn text(&self, obj: JsValue, heads: Option<Array>) -> Result<String, error::Get> {
        let (obj, _) = self.import(obj)?;
        if let Some(heads) = get_heads(heads)? {
            Ok(self.doc.text_at(&obj, &heads)?)
        } else {
            Ok(self.doc.text(&obj)?)
        }
    }

    pub fn spans(&self, obj: JsValue, heads: Option<Array>) -> Result<Array, error::GetSpans> {
        let (obj, _) = self.import(obj)?;
        let spans = if let Some(heads) = get_heads(heads)? {
            self.doc.spans_at(&obj, &heads)?
        } else {
            self.doc.spans(&obj)?
        };
        let cache = interop::ExportCache::new(self)?;
        Ok(interop::export_spans(self, cache, spans)?)
    }

    pub fn splice(
        &mut self,
        obj: JsValue,
        start: f64,
        delete_count: f64,
        text: JsValue,
    ) -> Result<(), error::Splice> {
        let (obj, obj_type) = self.import(obj)?;
        let start = start as usize;
        let delete_count = delete_count as isize;
        let vals = if let Some(t) = text.as_string() {
            if obj_type == am::ObjType::Text {
                self.doc.splice_text(&obj, start, delete_count, &t)?;
                return Ok(());
            } else {
                t.chars()
                    .map(|c| ScalarValue::Str(c.to_string().into()))
                    .collect::<Vec<_>>()
            }
        } else {
            let mut vals = vec![];
            if let Ok(array) = text.dyn_into::<Array>() {
                for (index, i) in array.iter().enumerate() {
                    let value = self
                        .import_scalar(&i, None)
                        .ok_or(error::Splice::ValueNotPrimitive(index))?;
                    vals.push(value);
                }
            }
            vals
        };
        if !vals.is_empty() {
            self.doc.splice(&obj, start, delete_count, vals)?;
        } else {
            // no vals given but we still need to call the text vs splice
            // bc utf16
            match obj_type {
                am::ObjType::List => {
                    self.doc.splice(&obj, start, delete_count, vals)?;
                }
                am::ObjType::Text => {
                    self.doc.splice_text(&obj, start, delete_count, "")?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[wasm_bindgen(js_name = updateText)]
    pub fn update_text(
        &mut self,
        obj: JsValue,
        new_text: JsValue,
    ) -> Result<(), error::UpdateText> {
        let (obj, obj_type) = self.import(obj)?;
        if !matches!(obj_type, am::ObjType::Text) {
            return Err(error::UpdateText::ObjectNotText);
        }
        if let Some(t) = new_text.as_string() {
            self.doc.update_text(&obj, t)?;
            Ok(())
        } else {
            Err(error::UpdateText::ValueNotString)
        }
    }

    #[wasm_bindgen(js_name = updateSpans)]
    pub fn update_spans(&mut self, obj: JsValue, args: JsValue) -> Result<(), error::UpdateSpans> {
        let (obj, obj_type) = self.import(obj)?;
        if !matches!(obj_type, am::ObjType::Text) {
            return Err(error::UpdateSpans::ObjectNotText);
        }
        let args = interop::import_update_spans_args(self, JS(args))?;
        self.doc.update_spans(&obj, args.0)?;
        Ok(())
    }

    pub fn push(
        &mut self,
        obj: JsValue,
        value: JsValue,
        datatype: JsValue,
    ) -> Result<(), error::Insert> {
        let (obj, _) = self.import(obj)?;
        let datatype = JS(datatype).try_into()?;
        let value = self
            .import_scalar(&value, datatype)
            .ok_or(error::Insert::ValueNotPrimitive)?;
        let index = self.doc.length(&obj);
        self.doc.insert(&obj, index, value)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = pushObject)]
    pub fn push_object(
        &mut self,
        obj: JsValue,
        value: JsValue,
    ) -> Result<Option<String>, error::InsertObject> {
        let (obj, _) = self.import(obj)?;
        let imported_obj = import_obj(&value, None)?;
        let index = self.doc.length(&obj);
        let opid = self
            .doc
            .insert_object(&obj, index, imported_obj.objtype())?;
        if let Some(s) = imported_obj.text() {
            self.doc.splice_text(&opid, 0, 0, s)?;
        } else {
            self.subset::<error::InsertObject, _>(&opid, imported_obj.subvals())?;
        }
        Ok(opid.to_string().into())
    }

    pub fn insert(
        &mut self,
        obj: JsValue,
        index: f64,
        value: JsValue,
        datatype: JsValue,
    ) -> Result<(), error::Insert> {
        let (obj, _) = self.import(obj)?;
        let datatype = JS(datatype).try_into()?;
        let value = self
            .import_scalar(&value, datatype)
            .ok_or(error::Insert::ValueNotPrimitive)?;
        self.doc.insert(&obj, index as usize, value)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = splitBlock)]
    pub fn split_block(
        &mut self,
        obj: JsValue,
        index: f64,
        args: JsValue,
    ) -> Result<(), error::SplitBlock> {
        let (obj, _) = self.import(obj)?;
        let block = self.doc.split_block(&obj, index as usize)?;
        let hydrate = match interop::js_val_to_hydrate(self, args) {
            Ok(val @ am::hydrate::Value::Map(_)) => val,
            _ => return Err(error::SplitBlock::InvalidArgs),
        };

        self.doc.update_object(&block, &hydrate)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = joinBlock)]
    pub fn join_block(&mut self, text: JsValue, index: usize) -> Result<(), error::Block> {
        let (text, _) = self.import(text)?;
        self.doc.join_block(&text, index)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = updateBlock)]
    pub fn update_block(
        &mut self,
        text: JsValue,
        index: usize,
        args: JsValue,
    ) -> Result<(), error::UpdateBlock> {
        let (text, _) = self.import(text)?;
        let new_block = self.doc.replace_block(&text, index)?;
        let new_value = interop::js_val_to_hydrate(self, args)?;
        if !matches!(new_value, am::hydrate::Value::Map(_)) {
            return Err(error::UpdateBlock::InvalidArgs);
        }
        self.doc.update_object(&new_block, &new_value)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = getBlock)]
    pub fn get_block(
        &mut self,
        text: JsValue,
        index: usize,
        heads: Option<Array>,
    ) -> Result<JsValue, error::GetBlock> {
        let (text, _) = self.import(text)?;
        let Some((Value::Object(am::ObjType::Map), id)) = self.doc.get(&text, index)? else {
            return Ok(JsValue::null());
        };

        let heads = get_heads(heads)?;
        let hydrated = if let Some(h) = heads {
            self.doc.hydrate(&id, Some(&h))?
        } else {
            self.doc.hydrate(&id, None)?
        };

        Ok(interop::export_hydrate(
            self,
            &interop::ExportCache::new(self).unwrap(),
            hydrated,
        ))
    }

    #[wasm_bindgen(js_name = insertObject)]
    pub fn insert_object(
        &mut self,
        obj: JsValue,
        index: f64,
        value: JsValue,
    ) -> Result<Option<String>, error::InsertObject> {
        let (obj, _) = self.import(obj)?;
        let imported_obj = import_obj(&value, None)?;
        let opid = self
            .doc
            .insert_object(&obj, index as usize, imported_obj.objtype())?;
        if let Some(s) = imported_obj.text() {
            self.doc.splice_text(&opid, 0, 0, s)?;
        } else {
            self.subset::<error::InsertObject, _>(&opid, imported_obj.subvals())?;
        }
        Ok(opid.to_string().into())
    }

    pub fn put(
        &mut self,
        obj: JsValue,
        prop: JsValue,
        value: JsValue,
        datatype: JsValue,
    ) -> Result<(), error::Insert> {
        let (obj, _) = self.import(obj)?;
        let prop = self.import_prop(prop)?;
        let datatype = JS(datatype).try_into()?;
        let value = self
            .import_scalar(&value, datatype)
            .ok_or(error::Insert::ValueNotPrimitive)?;
        self.doc.put(&obj, prop, value)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = putObject)]
    pub fn put_object(
        &mut self,
        obj: JsValue,
        prop: JsValue,
        value: JsValue,
    ) -> Result<JsValue, error::InsertObject> {
        let (obj, _) = self.import(obj)?;
        let prop = self.import_prop(prop)?;
        let imported_obj = import_obj(&value, None)?;
        let opid = self.doc.put_object(&obj, prop, imported_obj.objtype())?;
        if let Some(s) = imported_obj.text() {
            self.doc.splice_text(&opid, 0, 0, s)?;
        } else {
            self.subset::<error::InsertObject, _>(&opid, imported_obj.subvals())?;
        }
        Ok(opid.to_string().into())
    }

    fn subset<'a, E, I>(&mut self, obj: &am::ObjId, vals: I) -> Result<(), E>
    where
        I: IntoIterator<Item = (Cow<'a, am::Prop>, JsValue)>,
        E: From<automerge::AutomergeError>
            + From<interop::error::ImportObj>
            + From<interop::error::InvalidValue>,
    {
        for (p, v) in vals {
            let (value, subvals) = self.import_value(&v, None)?;
            //let opid = self.0.set(id, p, value)?;
            let opid = match (p.as_ref(), value) {
                (Prop::Map(s), Value::Object(objtype)) => {
                    Some(self.doc.put_object(obj, s, objtype)?)
                }
                (Prop::Map(s), Value::Scalar(scalar)) => {
                    self.doc.put(obj, s, scalar.into_owned())?;
                    None
                }
                (Prop::Seq(i), Value::Object(objtype)) => {
                    Some(self.doc.insert_object(obj, *i, objtype)?)
                }
                (Prop::Seq(i), Value::Scalar(scalar)) => {
                    self.doc.insert(obj, *i, scalar.into_owned())?;
                    None
                }
            };
            if let Some(opid) = opid {
                self.subset::<E, _>(&opid, SubValIter::Slice(subvals.as_slice().iter()))?;
            }
        }
        Ok(())
    }

    pub fn increment(
        &mut self,
        obj: JsValue,
        prop: JsValue,
        value: JsValue,
    ) -> Result<(), error::Increment> {
        let (obj, _) = self.import(obj)?;
        let prop = self.import_prop(prop)?;
        let value: f64 = value.as_f64().ok_or(error::Increment::ValueNotNumeric)?;
        self.doc.increment(&obj, prop, value as i64)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = get)]
    pub fn get(
        &self,
        obj: JsValue,
        prop: JsValue,
        heads: Option<Array>,
    ) -> Result<JsValue, error::Get> {
        let (obj, _) = self.import(obj)?;
        let prop = to_prop(prop);
        let heads = get_heads(heads)?;
        if let Ok(prop) = prop {
            let value = if let Some(h) = heads {
                self.doc.get_at(&obj, prop, &h)?
            } else {
                self.doc.get(&obj, prop)?
            };
            if let Some((value, id)) = value {
                match alloc(&value) {
                    (datatype, js_value) if datatype.is_scalar() => Ok(js_value),
                    _ => Ok(id.to_string().into()),
                }
            } else {
                Ok(JsValue::undefined())
            }
        } else {
            Ok(JsValue::undefined())
        }
    }

    #[wasm_bindgen(js_name = getWithType)]
    pub fn get_with_type(
        &self,
        obj: JsValue,
        prop: JsValue,
        heads: Option<Array>,
    ) -> Result<JsValue, error::Get> {
        let (obj, _) = self.import(obj)?;
        let prop = to_prop(prop);
        let heads = get_heads(heads)?;
        if let Ok(prop) = prop {
            let value = if let Some(h) = heads {
                self.doc.get_at(&obj, prop, &h)?
            } else {
                self.doc.get(&obj, prop)?
            };
            if let Some(value) = value {
                match &value {
                    (Value::Object(obj_type), obj_id) => {
                        let result = Array::new();
                        result.push(&obj_type.to_string().into());
                        result.push(&obj_id.to_string().into());
                        Ok(result.into())
                    }
                    (Value::Scalar(_), _) => {
                        let result = Array::new();
                        let (datatype, value) = alloc(&value.0);
                        result.push(&datatype.into());
                        result.push(&value);
                        Ok(result.into())
                    }
                }
            } else {
                Ok(JsValue::null())
            }
        } else {
            Ok(JsValue::null())
        }
    }

    #[wasm_bindgen(js_name = objInfo)]
    pub fn obj_info(&self, obj: JsValue, heads: Option<Array>) -> Result<Object, error::Get> {
        // fixme - import takes a path - needs heads to be accurate
        let (obj, _) = self.import(obj)?;
        let typ = self.doc.object_type(&obj)?;
        let result = Object::new();
        let parents = if let Some(heads) = get_heads(heads)? {
            self.doc.parents_at(&obj, &heads)
        } else {
            self.doc.parents(&obj)
        }?;
        js_set(&result, "id", obj.to_string())?;
        js_set(&result, "type", typ.to_string())?;
        if let Some(path) = parents.visible_path() {
            let path = interop::export_just_path(&path);
            js_set(&result, "path", &path)?;
        }
        Ok(result)
    }

    #[wasm_bindgen(js_name = getAll)]
    pub fn get_all(
        &self,
        obj: JsValue,
        arg: JsValue,
        heads: Option<Array>,
    ) -> Result<Array, error::Get> {
        let (obj, _) = self.import(obj)?;
        let result = Array::new();
        let prop = to_prop(arg);
        if let Ok(prop) = prop {
            let values = if let Some(heads) = get_heads(heads)? {
                self.doc.get_all_at(&obj, prop, &heads)
            } else {
                self.doc.get_all(&obj, prop)
            }?;
            for (value, id) in values {
                let sub = Array::new();
                let (datatype, js_value) = alloc(&value);
                sub.push(&datatype.into());
                if value.is_scalar() {
                    sub.push(&js_value);
                }
                sub.push(&id.to_string().into());
                result.push(&JsValue::from(&sub));
            }
        }
        Ok(result)
    }

    #[wasm_bindgen(js_name = enableFreeze)]
    pub fn enable_freeze(&mut self, enable: JsValue) -> Result<JsValue, JsValue> {
        let enable = enable
            .as_bool()
            .ok_or_else(|| to_js_err("must pass a bool to enableFreeze"))?;
        let old_freeze = self.freeze;
        self.freeze = enable;
        Ok(old_freeze.into())
    }

    #[wasm_bindgen(js_name = registerDatatype)]
    pub fn register_datatype(
        &mut self,
        datatype: JsValue,
        export_function: JsValue,
        import_function: JsValue,
    ) -> Result<(), value::InvalidDatatype> {
        let datatype = Datatype::try_from(datatype)?;
        let Ok(export_function) = export_function.dyn_into::<Function>() else {
            self.external_types.remove(&datatype);
            return Ok(());
        };
        let Ok(import_function) = import_function.dyn_into::<Function>() else {
            self.external_types.remove(&datatype);
            return Ok(());
        };
        self.external_types.insert(
            datatype,
            interop::ExternalTypeConstructor::new(export_function, import_function),
        );
        Ok(())
    }

    #[wasm_bindgen(js_name = applyPatches)]
    pub fn apply_patches(&mut self, object: JsValue, meta: JsValue) -> Result<JsValue, JsValue> {
        let (value, _patches) = self.apply_patches_impl(object, meta)?;
        Ok(value)
    }

    #[wasm_bindgen(js_name = applyAndReturnPatches)]
    pub fn apply_and_return_patches(
        &mut self,
        object: JsValue,
        meta: JsValue,
    ) -> Result<JsValue, JsValue> {
        let (value, patches) = self.apply_patches_impl(object, meta)?;

        let patches = interop::export_patches(&self.external_types, patches)?;

        let result = Object::new();
        js_set(&result, "value", value)?;
        js_set(&result, "patches", patches)?;
        Ok(result.into())
    }

    fn apply_patches_impl(
        &mut self,
        object: JsValue,
        meta: JsValue,
    ) -> Result<(JsValue, Vec<automerge::Patch>), JsValue> {
        let mut object = object
            .dyn_into::<Object>()
            .map_err(|_| error::ApplyPatch::NotObjectd)?;

        let shortcut = self.doc.diff_cursor().is_empty();
        let patches = self.doc.diff_incremental();

        let mut cache = interop::ExportCache::new(self)?;

        if shortcut {
            let value = cache.materialize(ROOT, Datatype::Map, None, &meta)?;
            return Ok((value, patches));
        }

        // even if there are no patches we may need to update the meta object
        // which requires that we update the object too
        if !meta.is_undefined() {
            let (_, cached_obj) = self.unwrap_object(&object, &mut cache, &meta)?;
            object = cached_obj.inner;
        }

        for p in &patches {
            object = self.apply_patch(object, p, &meta, &mut cache)?;
        }

        if self.freeze {
            Object::freeze(&object);
        }

        Ok((object.into(), patches))
    }

    #[wasm_bindgen(js_name = diffIncremental)]
    pub fn diff_incremental(&mut self) -> Result<Array, error::PopPatches> {
        // transactions send out observer updates as they occur, not waiting for them to be
        // committed.
        // If we pop the patches then we won't be able to revert them.

        let patches = self.doc.diff_incremental();
        let result = interop::export_patches(&self.external_types, patches)?;
        Ok(result)
    }

    #[wasm_bindgen(js_name = updateDiffCursor)]
    pub fn update_diff_cursor(&mut self) {
        self.doc.update_diff_cursor();
    }

    #[wasm_bindgen(js_name = resetDiffCursor)]
    pub fn reset_diff_cursor(&mut self) {
        self.doc.reset_diff_cursor();
    }

    pub fn diff(&mut self, before: Array, after: Array) -> Result<Array, error::Diff> {
        let before = get_heads(Some(before))?.unwrap();
        let after = get_heads(Some(after))?.unwrap();

        let patches = self.doc.diff(&before, &after);

        Ok(interop::export_patches(&self.external_types, patches)?)
    }

    pub fn isolate(&mut self, heads: Array) -> Result<(), error::Isolate> {
        let heads = get_heads(Some(heads))?.unwrap();
        self.doc.isolate(&heads);
        Ok(())
    }

    pub fn integrate(&mut self) {
        self.doc.integrate()
    }

    pub fn length(&self, obj: JsValue, heads: Option<Array>) -> Result<f64, error::Get> {
        let (obj, _) = self.import(obj)?;
        if let Some(heads) = get_heads(heads)? {
            Ok(self.doc.length_at(&obj, &heads) as f64)
        } else {
            Ok(self.doc.length(&obj) as f64)
        }
    }

    pub fn delete(&mut self, obj: JsValue, prop: JsValue) -> Result<(), error::Get> {
        let (obj, _) = self.import(obj)?;
        let prop = to_prop(prop)?;
        self.doc.delete(&obj, prop)?;
        Ok(())
    }

    pub fn save(&mut self) -> Uint8Array {
        Uint8Array::from(self.doc.save().as_slice())
    }

    #[wasm_bindgen(js_name = saveIncremental)]
    pub fn save_incremental(&mut self) -> Uint8Array {
        let bytes = self.doc.save_incremental();
        Uint8Array::from(bytes.as_slice())
    }

    #[wasm_bindgen(js_name=saveSince)]
    pub fn save_since(
        &mut self,
        heads: Array,
    ) -> Result<Uint8Array, interop::error::BadChangeHashes> {
        let heads = get_heads(Some(heads))?.unwrap_or(Vec::new());
        let bytes = self.doc.save_after(&heads);
        Ok(Uint8Array::from(bytes.as_slice()))
    }

    #[wasm_bindgen(js_name = saveNoCompress)]
    pub fn save_nocompress(&mut self) -> Uint8Array {
        let bytes = self.doc.save_nocompress();
        Uint8Array::from(bytes.as_slice())
    }

    #[wasm_bindgen(js_name = saveAndVerify)]
    pub fn save_and_verify(&mut self) -> Result<Uint8Array, error::Load> {
        let bytes = self.doc.save_and_verify()?;
        Ok(Uint8Array::from(bytes.as_slice()))
    }

    #[wasm_bindgen(js_name = loadIncremental)]
    pub fn load_incremental(&mut self, data: Uint8Array) -> Result<f64, error::Load> {
        let data = data.to_vec();
        let len = self.doc.load_incremental(&data)?;
        Ok(len as f64)
    }

    #[wasm_bindgen(js_name = applyChanges)]
    pub fn apply_changes(&mut self, changes: JsValue) -> Result<(), error::ApplyChangesError> {
        let changes: Vec<Change> = JS(changes).try_into()?;

        //for c in &changes {
        //am::log!("apply change: {:?}", c.raw_bytes());
        //}
        self.doc.apply_changes(changes)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = getChanges)]
    pub fn get_changes(&mut self, have_deps: JsValue) -> Result<Array, error::Get> {
        let deps: Vec<_> = JS(have_deps).try_into()?;
        let changes = self.doc.get_changes(&deps);
        let changes: Array = changes
            .iter()
            .map(|c| Uint8Array::from(c.raw_bytes()))
            .collect();
        Ok(changes)
    }

    #[wasm_bindgen(js_name = getChangesMeta)]
    pub fn get_changes_meta(&mut self, have_deps: JsValue) -> Result<Array, error::Get> {
        let deps: Vec<_> = JS(have_deps).try_into()?;
        let changes = self.doc.get_changes_meta(&deps);
        let changes: Array = changes.iter().map(JS::from).collect();
        Ok(changes)
    }

    #[wasm_bindgen(js_name = getChangeByHash)]
    pub fn get_change_by_hash(
        &mut self,
        hash: JsValue,
    ) -> Result<JsValue, interop::error::BadChangeHash> {
        let hash = JS(hash).try_into()?;
        let change = self.doc.get_change_by_hash(&hash);
        if let Some(c) = change {
            Ok(Uint8Array::from(c.raw_bytes()).into())
        } else {
            Ok(JsValue::null())
        }
    }

    #[wasm_bindgen(js_name = getChangeMetaByHash)]
    pub fn get_change_meta_by_hash(
        &mut self,
        hash: JsValue,
    ) -> Result<JsValue, interop::error::BadChangeHash> {
        let hash = JS(hash).try_into()?;
        let change_meta = self.doc.get_change_meta_by_hash(&hash);
        if let Some(c) = change_meta {
            Ok(JS::from(&c).0)
        } else {
            Ok(JsValue::null())
        }
    }

    #[wasm_bindgen(js_name = getDecodedChangeByHash)]
    pub fn get_decoded_change_by_hash(
        &mut self,
        hash: JsValue,
    ) -> Result<JsValue, error::GetDecodedChangeByHash> {
        let hash = JS(hash).try_into()?;
        let change = self.doc.get_change_by_hash(&hash);
        if let Some(c) = change {
            let change: am::ExpandedChange = c.decode();
            let serializer = serde_wasm_bindgen::Serializer::json_compatible();
            Ok(change.serialize(&serializer)?)
        } else {
            Ok(JsValue::null())
        }
    }

    #[wasm_bindgen(js_name = getChangesAdded)]
    pub fn get_changes_added(&mut self, other: &mut Automerge) -> Array {
        let changes = self.doc.get_changes_added(&mut other.doc);
        let changes: Array = changes
            .iter()
            .map(|c| Uint8Array::from(c.raw_bytes()))
            .collect();
        changes
    }

    #[wasm_bindgen(js_name = getHeads)]
    pub fn get_heads(&mut self) -> Array {
        let heads = self.doc.get_heads();
        AR::from(heads).into()
    }

    #[wasm_bindgen(js_name = getActorId)]
    pub fn get_actor_id(&self) -> String {
        self.doc.get_actor().to_string()
    }

    #[wasm_bindgen(js_name = getLastLocalChange)]
    pub fn get_last_local_change(&mut self) -> JsValue {
        if let Some(change) = self.doc.get_last_local_change() {
            Uint8Array::from(change.raw_bytes()).into()
        } else {
            JsValue::null()
        }
    }

    pub fn dump(&mut self) {
        self.doc.dump()
    }

    #[wasm_bindgen(js_name = getMissingDeps)]
    pub fn get_missing_deps(&mut self, heads: Option<Array>) -> Result<Array, error::Get> {
        let heads = get_heads(heads)?.unwrap_or_default();
        let deps = self.doc.get_missing_deps(&heads);
        let deps: Array = deps
            .iter()
            .map(|h| JsValue::from_str(&hex::encode(h.0)))
            .collect();
        Ok(deps)
    }

    #[wasm_bindgen(js_name = receiveSyncMessage)]
    pub fn receive_sync_message(
        &mut self,
        state: &mut SyncState,
        message: Uint8Array,
    ) -> Result<(), error::ReceiveSyncMessage> {
        let message = message.to_vec();
        //am::log!("receive sync message: {:?}", message.as_slice());
        let message = am::sync::Message::decode(message.as_slice())?;
        self.doc
            .sync()
            .receive_sync_message(&mut state.0, message)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = generateSyncMessage)]
    pub fn generate_sync_message(&mut self, state: &mut SyncState) -> JsValue {
        if let Some(message) = self.doc.sync().generate_sync_message(&mut state.0) {
            let message = message.encode();
            //am::log!("generate sync message: {:?}", message.as_slice());
            Uint8Array::from(message.as_slice()).into()
        } else {
            JsValue::null()
        }
    }

    #[wasm_bindgen(js_name = toJS)]
    pub fn to_js(&mut self, meta: JsValue) -> Result<JsValue, interop::error::Export> {
        let mut cache = interop::ExportCache::new(self)?;
        cache.materialize(ROOT, Datatype::Map, None, &meta)
    }

    pub fn materialize(
        &mut self,
        obj: JsValue,
        heads: Option<Array>,
        meta: JsValue,
    ) -> Result<JsValue, error::Materialize> {
        let (obj, obj_type) = self.import(obj).unwrap_or((ROOT, am::ObjType::Map));
        let heads = get_heads(heads)?;
        self.doc.update_diff_cursor();
        let mut cache = interop::ExportCache::new(self)?;
        Ok(cache.materialize(obj, obj_type.into(), heads.as_deref(), &meta)?)
    }

    #[wasm_bindgen(js_name = getCursor)]
    pub fn get_cursor(
        &mut self,
        obj: JsValue,
        position: JsValue,
        heads: Option<Array>,
        move_cursor: JsValue,
    ) -> Result<String, error::Cursor> {
        let (obj, obj_type) = self.import(obj).unwrap_or((ROOT, am::ObjType::Map));
        if obj_type != am::ObjType::Text {
            return Err(error::Cursor::InvalidObjType(obj_type));
        }

        let heads = get_heads(heads)?;

        let position: CursorPosition = JS(position)
            .try_into()
            .map_err(|_| error::Cursor::InvalidCursorPosition)?;

        // TODO do we want this?

        // convert positions >= string.length into `CursorPosition::End`
        // note: negative indices are converted to `CursorPosition::Start` in
        // `impl TryFrom<JS> for CursorPosition`
        let len = match heads {
            Some(ref heads) => self.doc.length_at(&obj, heads),
            None => self.doc.length(&obj),
        };

        let position = match position {
            CursorPosition::Index(i) if i >= len => CursorPosition::End,
            _ => position,
        };

        let cursor = if move_cursor.is_undefined() {
            self.doc.get_cursor(obj, position, heads.as_deref())?
        } else {
            let move_cursor = JS(move_cursor)
                .try_into()
                .map_err(|_| error::Cursor::InvalidMoveCursor)?;

            self.doc
                .get_cursor_moving(obj, position, heads.as_deref(), move_cursor)?
        };

        Ok(cursor.to_string())
    }

    #[wasm_bindgen(js_name = getCursorPosition)]
    pub fn get_cursor_position(
        &mut self,
        obj: JsValue,
        cursor: JsValue,
        heads: Option<Array>,
    ) -> Result<f64, error::Cursor> {
        let (obj, obj_type) = self.import(obj).unwrap_or((ROOT, am::ObjType::Map));
        if obj_type != am::ObjType::Text {
            return Err(error::Cursor::InvalidObjType(obj_type));
        }
        let cursor = cursor.as_string().ok_or(error::Cursor::InvalidCursor)?;
        let cursor = am::Cursor::try_from(cursor)?;
        let heads = get_heads(heads)?;
        let position = self
            .doc
            .get_cursor_position(obj, &cursor, heads.as_deref())?;
        Ok(position as f64)
    }

    #[wasm_bindgen(js_name = emptyChange)]
    pub fn empty_change(&mut self, message: Option<String>, time: Option<f64>) -> JsValue {
        let time = time.map(|f| f as i64);
        let options = CommitOptions { message, time };
        let hash = self.doc.empty_change(options);
        JsValue::from_str(&hex::encode(hash))
    }

    pub fn mark(
        &mut self,
        obj: JsValue,
        range: JsValue,
        name: JsValue,
        value: JsValue,
        datatype: JsValue,
    ) -> Result<(), error::Mark> {
        let (obj, _) = self.import(obj)?;

        let range = range
            .dyn_into::<Object>()
            .map_err(|_| error::Mark::InvalidRange)?;

        let start = js_get(&range, "start").map_err(|_| error::Mark::InvalidStart)?;
        let start = start.try_into().map_err(|_| error::Mark::InvalidStart)?;

        let end = js_get(&range, "end").map_err(|_| error::Mark::InvalidEnd)?;
        let end = end.try_into().map_err(|_| error::Mark::InvalidEnd)?;

        let expand = js_get(&range, "expand").ok();
        let expand = expand.map(|s| s.try_into()).transpose()?;
        let expand = expand.unwrap_or_default();

        let name = name.as_string().ok_or(error::Mark::InvalidName)?;

        let datatype = JS(datatype).try_into()?;
        let value = self
            .import_scalar(&value, datatype)
            .ok_or_else(|| error::Mark::InvalidValue)?;

        self.doc
            .mark(&obj, Mark::new(name, value, start, end), expand)?;
        Ok(())
    }

    pub fn unmark(
        &mut self,
        obj: JsValue,
        range: JsValue,
        name: JsValue,
    ) -> Result<(), error::Mark> {
        self.mark(obj, range, name, JsValue::NULL, JsValue::from_str("null"))
    }

    pub fn marks(&mut self, obj: JsValue, heads: Option<Array>) -> Result<JsValue, JsValue> {
        let (obj, _) = self.import(obj)?;
        let heads = get_heads(heads)?;
        let marks = if let Some(heads) = heads {
            self.doc.marks_at(obj, &heads).map_err(to_js_err)?
        } else {
            self.doc.marks(obj).map_err(to_js_err)?
        };
        let result = Array::new();
        for m in marks {
            let mark = Object::new();
            let (_datatype, value) = alloc(&m.value().clone().into());
            js_set(&mark, "name", m.name())?;
            js_set(&mark, "value", value)?;
            js_set(&mark, "start", m.start as i32)?;
            js_set(&mark, "end", m.end as i32)?;
            result.push(&mark.into());
        }
        Ok(result.into())
    }

    #[wasm_bindgen(js_name = marksAt)]
    pub fn marks_at(
        &mut self,
        obj: JsValue,
        index: f64,
        heads: Option<Array>,
    ) -> Result<Object, JsValue> {
        let (obj, _) = self.import(obj)?;
        let heads = get_heads(heads)?;
        let marks = self
            .doc
            .get_marks(obj, index as usize, heads.as_deref())
            .map_err(to_js_err)?;
        let result = Object::new();
        for (mark, value) in marks.iter() {
            let (_datatype, value) = alloc(&value.into());
            js_set(&result, mark, value)?;
        }
        Ok(result)
    }

    pub(crate) fn text_at(
        &self,
        obj: &am::ObjId,
        heads: Option<&[am::ChangeHash]>,
    ) -> Result<String, am::AutomergeError> {
        if let Some(heads) = heads {
            Ok(self.doc.text_at(obj, heads)?)
        } else {
            Ok(self.doc.text(obj)?)
        }
    }

    #[wasm_bindgen(js_name = hasOurChanges)]
    pub fn has_our_changes(&mut self, state: &mut SyncState) -> JsValue {
        self.doc.has_our_changes(&state.0).into()
    }

    #[wasm_bindgen(js_name = topoHistoryTraversal)]
    pub fn topo_history_traversal(&mut self) -> JsValue {
        let hashes = self
            .doc
            .get_changes(&[])
            .into_iter()
            .map(|c| c.hash())
            .collect::<Vec<_>>();
        AR::from(hashes).into()
    }

    #[wasm_bindgen(js_name = stats)]
    pub fn stats(&mut self) -> JsValue {
        let stats = self.doc.stats();
        let result = Object::new();
        let num_changes = JsValue::from(stats.num_changes as usize);
        let num_ops = JsValue::from(stats.num_ops as usize);
        let num_actors = JsValue::from(stats.num_actors as usize);
        let cargo_package_name = JsValue::from(stats.cargo_package_name);
        let cargo_package_version = JsValue::from(stats.cargo_package_version);
        let rustc_version = JsValue::from(stats.rustc_version);
        js_set(&result, "numChanges", num_changes).unwrap();
        js_set(&result, "numOps", num_ops).unwrap();
        js_set(&result, "numActors", num_actors).unwrap();
        js_set(&result, "cargoPackageName", cargo_package_name).unwrap();
        js_set(&result, "cargoPackageVersion", cargo_package_version).unwrap();
        js_set(&result, "rustcVersion", rustc_version).unwrap();
        result.into()
    }
}

#[wasm_bindgen(js_name = create)]
pub fn init(options: JsValue) -> Result<Automerge, error::BadActorId> {
    console_error_panic_hook::set_once();
    let actor = js_get(&options, "actor").ok().and_then(|a| a.as_string());
    Automerge::new(actor)
}

#[wasm_bindgen(js_name = load)]
pub fn load(data: Uint8Array, options: JsValue) -> Result<Automerge, error::Load> {
    let data = data.to_vec();
    let actor = js_get(&options, "actor").ok().and_then(|a| a.as_string());
    let unchecked = js_get(&options, "unchecked")
        .ok()
        .and_then(|v1| v1.as_bool())
        .unwrap_or(false);
    let verification_mode = if unchecked {
        VerificationMode::DontCheck
    } else {
        VerificationMode::Check
    };
    let allow_missing_deps = js_get(&options, "allowMissingDeps")
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let on_partial_load = if allow_missing_deps {
        OnPartialLoad::Ignore
    } else {
        OnPartialLoad::Error
    };
    let string_migration = if js_get(&options, "convertImmutableStringsToText")
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        StringMigration::ConvertToText
    } else {
        StringMigration::NoMigration
    };
    let mut doc = am::AutoCommit::load_with_options(
        &data,
        am::LoadOptions::new()
            .on_partial_load(on_partial_load)
            .verification_mode(verification_mode)
            .migrate_strings(string_migration),
    )?
    .with_text_rep(TextRepresentation::String(TextEncoding::Utf16CodeUnit));
    if let Some(s) = actor {
        let actor =
            automerge::ActorId::from(hex::decode(s).map_err(error::BadActorId::from)?.to_vec());
        doc.set_actor(actor);
    }
    Ok(Automerge {
        doc,
        freeze: false,
        external_types: HashMap::default(),
    })
}

#[wasm_bindgen(js_name = encodeChange)]
pub fn encode_change(change: JsValue) -> Result<Uint8Array, error::EncodeChange> {
    // Alex: Technically we should be using serde_wasm_bindgen::from_value instead of into_serde.
    // Unfortunately serde_wasm_bindgen::from_value fails for some inscrutable reason, so instead
    // we use into_serde (sorry to future me).
    #[allow(deprecated)]
    let change: am::ExpandedChange = change.into_serde()?;
    let change: Change = change.into();
    Ok(Uint8Array::from(change.raw_bytes()))
}

#[wasm_bindgen(js_name = decodeChange)]
pub fn decode_change(change: Uint8Array) -> Result<JsValue, error::DecodeChange> {
    let change = Change::from_bytes(change.to_vec())?;
    let change: am::ExpandedChange = change.decode();
    let serializer = serde_wasm_bindgen::Serializer::json_compatible();
    Ok(change.serialize(&serializer)?)
}

#[wasm_bindgen(js_name = initSyncState)]
pub fn init_sync_state() -> SyncState {
    SyncState(am::sync::State::new())
}

// this is needed to be compatible with the automerge-js api
#[wasm_bindgen(js_name = importSyncState)]
pub fn import_sync_state(state: JsValue) -> Result<SyncState, interop::error::BadSyncState> {
    Ok(SyncState(JS(state).try_into()?))
}

// this is needed to be compatible with the automerge-js api
#[wasm_bindgen(js_name = exportSyncState)]
pub fn export_sync_state(state: &SyncState) -> JsValue {
    JS::from(state.0.clone()).into()
}

#[wasm_bindgen(js_name = encodeSyncMessage)]
pub fn encode_sync_message(message: JsValue) -> Result<Uint8Array, interop::error::BadSyncMessage> {
    let message: am::sync::Message = JS(message).try_into()?;
    Ok(Uint8Array::from(message.encode().as_slice()))
}

#[wasm_bindgen(js_name = decodeSyncMessage)]
pub fn decode_sync_message(msg: Uint8Array) -> Result<JsValue, error::BadSyncMessage> {
    let data = msg.to_vec();
    let msg = am::sync::Message::decode(&data)?;
    let heads = AR::from(msg.heads.as_slice());
    let need = AR::from(msg.need.as_slice());
    let changes = AR::from(&msg.changes);
    let have = AR::from(msg.have.as_slice());
    let obj = Object::new().into();
    // SAFETY: we just created this object
    js_set(&obj, "heads", heads).unwrap();
    js_set(&obj, "need", need).unwrap();
    js_set(&obj, "have", have).unwrap();
    js_set(&obj, "changes", changes).unwrap();

    match msg.version {
        am::sync::MessageVersion::V1 => {
            js_set(&obj, "type", JsValue::from_str("v1")).unwrap();
        }
        am::sync::MessageVersion::V2 => {
            js_set(&obj, "type", JsValue::from_str("v2")).unwrap();
        }
    };

    if let Some(caps) = msg.supported_capabilities {
        let caps = AR::from(caps.as_slice());
        js_set(&obj, "supportedCapabilities", caps).unwrap();
    }

    Ok(obj)
}

#[wasm_bindgen(js_name = encodeSyncState)]
pub fn encode_sync_state(state: &SyncState) -> Uint8Array {
    Uint8Array::from(state.0.encode().as_slice())
}

#[wasm_bindgen(js_name = decodeSyncState)]
pub fn decode_sync_state(data: Uint8Array) -> Result<SyncState, sync::DecodeSyncStateErr> {
    SyncState::decode(data)
}

struct UpdateSpansArgs(Vec<am::BlockOrText<'static>>);

pub mod error {
    use automerge::{AutomergeError, ObjType};
    use js_sys::RangeError;
    use wasm_bindgen::JsValue;

    use crate::interop::{
        self,
        error::{BadChangeHashes, BadJSChanges},
    };

    #[derive(Debug, thiserror::Error)]
    #[error("could not parse Actor ID as a hex string: {0}")]
    pub struct BadActorId(#[from] hex::FromHexError);

    impl From<BadActorId> for JsValue {
        fn from(s: BadActorId) -> Self {
            RangeError::new(&s.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum ApplyChangesError {
        #[error(transparent)]
        DecodeChanges(#[from] BadJSChanges),
        #[error("error applying changes: {0}")]
        Apply(#[from] AutomergeError),
    }

    impl From<ApplyChangesError> for JsValue {
        fn from(e: ApplyChangesError) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Fork {
        #[error(transparent)]
        BadActor(#[from] BadActorId),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        BadChangeHashes(#[from] BadChangeHashes),
    }

    impl From<Fork> for JsValue {
        fn from(f: Fork) -> Self {
            RangeError::new(&f.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error(transparent)]
    pub struct Merge(#[from] AutomergeError);

    impl From<Merge> for JsValue {
        fn from(e: Merge) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Get {
        #[error("invalid object ID: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error("object not visible")]
        NotVisible,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error("bad heads: {0}")]
        BadHeads(#[from] interop::error::BadChangeHashes),
        #[error(transparent)]
        InvalidProp(#[from] interop::error::InvalidProp),
        #[error(transparent)]
        ExportError(#[from] interop::error::SetProp),
    }

    impl From<Get> for JsValue {
        fn from(e: Get) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Splice {
        #[error("invalid object ID: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error("value at {0} in values to insert was not a primitive")]
        ValueNotPrimitive(usize),
    }

    impl From<Splice> for JsValue {
        fn from(e: Splice) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum UpdateText {
        #[error("invalid object ID: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error("object was not a text object")]
        ObjectNotText,
        #[error("update_text is only availalbe for the string representation of text objects")]
        TextRepNotString,
        #[error("value passed to update_text was not a string")]
        ValueNotString,
    }

    impl From<UpdateText> for JsValue {
        fn from(e: UpdateText) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum UpdateSpans {
        #[error("invalid object ID: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error("object was not a text object")]
        ObjectNotText,
        #[error(transparent)]
        InvalidArgs(#[from] interop::error::InvalidUpdateSpansArgs),
        #[error("update_text is only availalbe for the string representation of text objects")]
        TextRepNotString,
    }

    impl From<UpdateSpans> for JsValue {
        fn from(e: UpdateSpans) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Insert {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error("the value to insert was not a primitive")]
        ValueNotPrimitive,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        InvalidProp(#[from] interop::error::InvalidProp),
        #[error(transparent)]
        InvalidValue(#[from] interop::error::InvalidValue),
        #[error(transparent)]
        InvalidDatatype(#[from] crate::value::InvalidDatatype),
    }

    impl From<Insert> for JsValue {
        fn from(e: Insert) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Block {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error("block name must be a string")]
        InvalidName,
        #[error("block parents must be an array of strings")]
        InvalidParents,
        #[error("invalid cursor")]
        InvalidCursor,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
    }

    impl From<Block> for JsValue {
        fn from(e: Block) -> Self {
            JsValue::from(e.to_string())
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum InsertObject {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error("the value to insert must be an object")]
        ValueNotObject,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        InvalidProp(#[from] interop::error::InvalidProp),
        #[error(transparent)]
        InvalidValue(#[from] interop::error::InvalidValue),
    }

    impl From<InsertObject> for JsValue {
        fn from(e: InsertObject) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Increment {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        InvalidProp(#[from] interop::error::InvalidProp),
        #[error("value was not numeric")]
        ValueNotNumeric,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
    }

    impl From<Increment> for JsValue {
        fn from(e: Increment) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum BadSyncMessage {
        #[error("could not decode sync message: {0}")]
        ReadMessage(#[from] automerge::sync::ReadMessageError),
    }

    impl From<BadSyncMessage> for JsValue {
        fn from(e: BadSyncMessage) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum ApplyPatch {
        #[error(transparent)]
        Interop(#[from] interop::error::ApplyPatch),
        #[error(transparent)]
        Export(#[from] interop::error::Export),
        #[error("patch was not an object")]
        NotObjectd,
        #[error("error calling patch callback: {0:?}")]
        PatchCallback(JsValue),
    }

    impl From<ApplyPatch> for JsValue {
        fn from(e: ApplyPatch) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("unable to build patches: {0}")]
    pub struct PopPatches(#[from] interop::error::Export);

    impl From<PopPatches> for JsValue {
        fn from(e: PopPatches) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Diff {
        #[error(transparent)]
        Export(#[from] interop::error::Export),
        #[error("bad heads: {0}")]
        Heads(#[from] interop::error::BadChangeHashes),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
    }

    impl From<Diff> for JsValue {
        fn from(e: Diff) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Isolate {
        #[error("bad heads: {0}")]
        Heads(#[from] interop::error::BadChangeHashes),
    }

    impl From<Isolate> for JsValue {
        fn from(e: Isolate) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Materialize {
        #[error(transparent)]
        Export(#[from] interop::error::Export),
        #[error("bad heads: {0}")]
        Heads(#[from] interop::error::BadChangeHashes),
    }

    impl From<Materialize> for JsValue {
        fn from(e: Materialize) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Cursor {
        //#[error(transparent)]
        //Export(#[from] interop::error::Export),
        #[error("invalid cursor")]
        InvalidCursor,
        #[error("invalid position - must be an index, 'start' or 'end'")]
        InvalidCursorPosition,
        #[error("invalid move - must be 'before' or 'after'")]
        InvalidMoveCursor,
        #[error("cursors only valid on text - obj type: {0}")]
        InvalidObjType(ObjType),
        #[error("bad heads: {0}")]
        Heads(#[from] interop::error::BadChangeHashes),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
    }

    impl From<Cursor> for JsValue {
        fn from(e: Cursor) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum ReceiveSyncMessage {
        #[error(transparent)]
        Decode(#[from] automerge::sync::ReadMessageError),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
    }

    impl From<ReceiveSyncMessage> for JsValue {
        fn from(e: ReceiveSyncMessage) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Load {
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        BadActor(#[from] BadActorId),
    }

    impl From<Load> for JsValue {
        fn from(e: Load) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("Unable to read JS change: {0}")]
    pub struct EncodeChange(#[from] serde_json::Error);

    impl From<EncodeChange> for JsValue {
        fn from(e: EncodeChange) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum DecodeChange {
        #[error(transparent)]
        Load(#[from] automerge::LoadChangeError),
        #[error(transparent)]
        Serialize(#[from] serde_wasm_bindgen::Error),
    }

    impl From<DecodeChange> for JsValue {
        fn from(e: DecodeChange) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Mark {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        Expand(#[from] interop::error::BadExpand),
        #[error("Invalid mark name")]
        InvalidName,
        #[error("Invalid mark value")]
        InvalidValue,
        #[error("start must be a number")]
        InvalidStart,
        #[error("end must be a number")]
        InvalidEnd,
        #[error("range must be an object")]
        InvalidRange,
        #[error(transparent)]
        InvalidDatatype(#[from] crate::value::InvalidDatatype),
    }

    impl From<Mark> for JsValue {
        fn from(e: Mark) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum SplitBlock {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error("the block value must be a map")]
        InvalidArgs,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        UpdateObject(#[from] automerge::error::UpdateObjectError),
    }

    impl From<SplitBlock> for JsValue {
        fn from(e: SplitBlock) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum UpdateBlock {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error("the updated block args must be a map")]
        InvalidArgs,
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        Update(#[from] automerge::error::UpdateObjectError),
        #[error("invalid value")]
        InvalidValue(#[from] interop::error::JsValToHydrate),
    }

    impl From<UpdateBlock> for JsValue {
        fn from(e: UpdateBlock) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum GetBlock {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        Set(#[from] interop::error::SetProp),
        #[error(transparent)]
        Export(#[from] interop::error::Export),
        #[error(transparent)]
        BadHeads(#[from] interop::error::BadChangeHashes),
    }

    impl From<GetBlock> for JsValue {
        fn from(e: GetBlock) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum GetSpans {
        #[error("invalid object id: {0}")]
        ImportObj(#[from] interop::error::ImportObj),
        #[error(transparent)]
        BadHeads(#[from] interop::error::BadChangeHashes),
        #[error(transparent)]
        Export(#[from] interop::error::Export),
        #[error(transparent)]
        Automerge(#[from] AutomergeError),
        #[error(transparent)]
        Set(#[from] interop::error::SetProp),
    }

    impl From<GetSpans> for JsValue {
        fn from(e: GetSpans) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum GetDecodedChangeByHash {
        #[error(transparent)]
        BadChangeHash(#[from] super::interop::error::BadChangeHash),
        #[error(transparent)]
        SerdeWasm(#[from] serde_wasm_bindgen::Error),
    }

    impl From<GetDecodedChangeByHash> for JsValue {
        fn from(e: GetDecodedChangeByHash) -> Self {
            RangeError::new(&e.to_string()).into()
        }
    }
}
