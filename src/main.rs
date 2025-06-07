use std::fmt;
use std::ptr;
use std::env;
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions, read};
use std::os::fd::AsRawFd;
use std::io::{self, Read, Write, PipeWriter, PipeReader, BufRead, BufReader, ErrorKind, Cursor};
use std::process::{Command, Stdio, exit};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::path::{Path, PathBuf};
use std::os::unix::ffi::OsStringExt;
use std::os::fd::{FromRawFd, RawFd};
use std::ffi::OsString;
use std::str::Chars;
use std::iter::Peekable;

use regex::Regex;
extern crate libc;
use libc::{fork, waitpid, pid_t, WIFEXITED, WEXITSTATUS};
use glob::{glob, Pattern};
use tempfile::tempfile;

union Val {
    id: usize,
    num: isize,
    sym: *mut Sym,
    var: *mut Var,
    cell: *mut Cons,
    func: Primitive,
    fat: *mut FatPtr,
    mem: *mut Mem,
}
#[derive (PartialEq, Copy, Clone)]
enum Mode {Single, Multi, DoMulti, Set, DoSet, None}
impl Mode {
    #[inline(always)]
    fn for_special_form(self) -> Self {
        if self == Mode::None {
            Mode::Single
        } else {
            self
        }
    }
    fn for_progn(self) -> Self {
        match self {
            Mode::Multi => Mode::DoMulti,
            Mode::Set => Mode::DoSet,
            _ => Mode::Single
        }
    }
    fn for_return(self) -> Self {
        match self {
            Mode::DoMulti => Mode::Multi,
            Mode::DoSet => Mode::Set,
            _ => Mode::Single
        }
    }
}

type Primitive = fn(&mut Env, Mode, &Val) -> Result<bool, Exception>;
const TAG_MASK: usize = 31;
const VAR: usize = 0;
const NUM: usize = 1;
const FUNC: usize = 2;
const CELL: usize = 8;
const SYM: usize = 16;
const FAT: usize = 24;

impl Val {
    #[inline(always)]
    fn to_path<'a>(&'a self) -> Result<Cow<'a, Path>, ()> {
        self.try_into()
    }
    #[inline(always)]
    fn to_str<'a>(&'a self) -> Result<Cow<'a, str>, ()> {
        self.try_into()
    }
    #[inline(always)]
    fn int(&self) -> Option<isize> {
        unsafe {
            match self.id & TAG_MASK {
                SYM => (&*(*self.var).name).to_string_lossy().parse().ok(),
                CELL => None,
                FAT => {
                    if self.is_float() {
                        match self.fat() {
                            Fat::Float(x) => Some(*x as isize),
                            _ => panic!()
                        }
                    } else {
                        None
                    }
                }
                VAR => (&*(*self.sym).name).to_string_lossy().parse().ok(),
                _ => {
                    let result = self.num >> 1;
                    Some(result)
                }
            }
        }
    }
    #[inline(always)]
    fn is_sym (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                SYM => true,
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_num (&self) -> bool {
        unsafe {
            self.id & NUM == 1
        }
    }
    #[inline(always)]
    fn is_cell (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                CELL => true,
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_var (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                VAR => true,
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_str (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                VAR => &self.var().val == self,
                _ => false
            }
        }
    }
    #[inline(always)]
    fn copy (&self) -> Val {
        unsafe {
            Val{id: self.id}
        }
    }
    #[inline(always)]
    fn is_fat (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => true,
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_buf (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::Buf(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_chars (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::Chars(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_file (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::File(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_dict (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::Dict(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_piper (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::PipeR(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_pipew (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::PipeW(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_captured (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::Captured(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn is_float (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::Float(_) => true,
                        _ => false
                    };
                    std::mem::forget(fat);
                    result
                }
                _ => false
            }
        }
    }
    #[inline(always)]
    fn add_tag (self, tag: usize) -> Self {
        unsafe {
            let result = Val {id: self.id | tag};
            std::mem::forget(self);
            result
        }
    }
    #[inline(always)]
    fn remove_tag (self, tag: usize) -> Self {
        unsafe {
            let result = Val {id: self.id & !tag};
            std::mem::forget(self);
            result
        }
    }
    #[inline(always)]
    fn new () -> Self {
        NEXT_CELL.with(|next_cell|
            if next_cell.get().is_null() {
                Pool::new().add_list();
                Val::new()
            } else {
                unsafe {
                    Self {mem: next_cell.replace((*next_cell.get()).next)}
                }
            }
        )
    }
    fn init_value_of(mut self, p: &mut Val) {
        std::mem::swap(&mut self, p);
        std::mem::forget(self);
    }
    fn cons(self, cdr: Val) -> Val {
        unsafe {
            let result = Val::new().add_tag(CELL);
            self.init_value_of(&mut (*result.cell).car);
            cdr.init_value_of(&mut (*result.cell).cdr);
            (*result.cell).count = 1;
            result
        }
    }
    fn capture(self) -> Val {
        if self.is_captured() {
            self
        } else {
            unsafe {
                let result = Val::new();
                let tmp = std::mem::replace(&mut (*result.fat).val, Fat::Captured(self));
                std::mem::forget(tmp);
                (*result.fat).count = 1;
                result.add_tag(FAT)
            }
        }
    }
    fn fat_type_of(fat: &Fat) -> &'static str {
        match fat {
            Fat::Buf(_) => "buffered",
            Fat::Chars(_) => "characters",
            Fat::Captured(x) => x.type_of(),
            Fat::Float(_) => "float",
            Fat::File(_) => "file",
            Fat::PipeR(_) => "pipe",
            Fat::PipeW(_) => "pipe",
            Fat::Dict(_) => "dictionary",
            Fat::Nothing => "none",
        }
    }
    fn type_of(&self) -> &'static str {
        unsafe {
            match self.id & TAG_MASK {
                VAR => {
                    if self.var().val.id == self.id {
                        "string"
                    } else {
                        "variable"
                    }
                }
                CELL => "list",
                SYM => "symbol",
                FAT => Self::fat_type_of(self.fat()),
                _ => {
                    if self.id & 1 == 1 {
                        "integer"
                    } else {
                        "primitive"
                    }
                }
            }
        }
    }


    #[inline(always)]
    fn car(&self) -> &Val {
        if !self.is_cell() {
            panic!();
        }
        unsafe {
            &(*self.cell).car
        }
    }
    #[inline(always)]
    fn cdr(&self) -> &Val {
        if !self.is_cell() {
            panic!();
        }
        unsafe {
            &(*self.cell).cdr
        }
    }
    #[inline(always)]
    fn car_mut(&self) -> &mut Val {
        if !self.is_cell() {
            panic!();
        }
        unsafe {
            &mut(*self.cell).car
        }
    }
    #[inline(always)]
    fn cdr_mut(&self) -> &mut Val {
        if !self.is_cell() {
            panic!();
        }
        unsafe {
            &mut(*self.cell).cdr
        }
    }
    #[inline(always)]
    fn var(&self) -> &mut Var {
        if !self.is_var() {
            panic!();
        }
        unsafe {
            &mut(*self.var)
        }
    }
    #[inline(always)]
    fn sym(&self) -> &mut Sym {
        if !self.is_sym() {
            panic!();
        }
        unsafe {
            &mut(*self.sym)
        }
    }
    fn fat(&self) -> &mut Fat {
        if !self.is_fat() {
            panic!();
        }
        unsafe{
            let tmp = self.copy().remove_tag(FAT);
            let mut result = &mut (*tmp.fat).val;
            std::mem::forget(tmp);
            result
        }
    }
    #[inline(always)]
    fn buf(&mut self) -> &mut dyn BufRead {
        if !self.is_buf() {
            panic!();
        }
        match self.fat() {
            Fat::Buf(x) => x.as_mut(),
            _ => panic!()
        }
    }
    #[inline(always)]
    fn chars(&mut self) -> &mut dyn CharsAPI {
        if !self.is_chars() {
            panic!();
        }
        match self.fat() {
            Fat::Chars(x) => x.as_mut(),
            _ => panic!()
        }
    }
    #[inline(always)]
    fn file(&self) -> &mut File {
        if !self.is_file() {
            panic!();
        }
        match self.fat() {
            Fat::File(x) => x.as_mut(),
            _ => panic!()
        }
    }
    #[inline(always)]
    fn clone_file(&self) -> File {
        if !self.is_file() {
            panic!();
        }
        match self.fat() {
            Fat::File(x) => x.try_clone().unwrap(),
            _ => panic!()
        }
    }
    #[inline(always)]
    fn move_file(&self) -> File {
        if !self.is_file() {
            panic!();
        }
        let mut tmp = Fat::Nothing;
        std::mem::swap(&mut tmp, &mut self.fat());
        match tmp {
            Fat::File(file) => *file,
            _ => panic!()
        }
    }
    #[inline(always)]
    fn clone_piper(&self) -> PipeReader {
        if !self.is_piper() {
            panic!();
        }
        let mut tmp = Fat::Nothing;
        std::mem::swap(&mut tmp, &mut self.fat());
        match tmp {
            Fat::PipeR(r) => r.try_clone().unwrap(),
            _ => panic!()
        }
    }
    #[inline(always)]
    fn move_piper(&self) -> PipeReader {
        if !self.is_piper() {
            panic!();
        }
        let mut tmp = Fat::Nothing;
        std::mem::swap(&mut tmp, &mut self.fat());
        match tmp {
            Fat::PipeR(r) => *r,
            _ => panic!()
        }
    }
    #[inline(always)]
    fn clone_pipew(&self) -> PipeWriter {
        if !self.is_pipew() {
            panic!();
        }
        let mut tmp = Fat::Nothing;
        std::mem::swap(&mut tmp, &mut self.fat());
        match tmp {
            Fat::PipeW(w) => w.try_clone().unwrap(),
            _ => panic!()
        }
    }
    fn to_stdio(&self, env: &mut Env) -> Result<Stdio, Exception> {
        if self.is_file() {
            Ok(Stdio::from(self.clone_file()))
        } else if self.is_piper() {
            Ok(Stdio::from(self.clone_piper()))
        } else if self.is_pipew() {
            Ok(Stdio::from(self.clone_pipew()))
        } else {
            Err(env.type_err("shino", self, "fd"))
        }
    }
    #[inline(always)]
    fn captured(&self) -> &mut Val {
        if !self.is_captured() {
            panic!();
        }
        unsafe {
            match self.fat() {
                Fat::Captured(x) => x,
                _ => panic!()
            }
        }
    }
    #[inline(always)]
    fn piper(&self) -> &mut PipeReader {
        if !self.is_piper() {
            panic!();
        }
        unsafe {
            match self.fat() {
                Fat::PipeR(x) => x.as_mut(),
                _ => panic!()
            }
        }
    }
    #[inline(always)]
    fn pipew(&self) -> &mut PipeWriter {
        if !self.is_pipew() {
            panic!();
        }
        unsafe {
            match self.fat() {
                Fat::PipeW(x) => x.as_mut(),
                _ => panic!()
            }
        }
    }
    #[inline(always)]
    fn dict(&self) -> &mut Box<HashMap<PathBuf, Val>> {
        if !self.is_dict() {
            panic!();
        }
        unsafe {
            match self.fat() {
                Fat::Dict(x) => x,
                _ => panic!()
            }
        }
    }
    #[inline(always)]
    fn is_nil(&self) -> bool {
        NIL.with(|x| unsafe {
            self == x.get().unwrap()
        })
    }
    #[inline(always)]
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> Result<usize, std::io::Error> {
        let mut len = 0;
        let mut buffer = [0u8; 1];
        if self.is_file() {
            let file = self.file();
            loop {
                let l = file.read(&mut buffer)?;
                if l == 0 || buffer[0] == byte {
                    return Ok(len);
                } else {
                    len += l;
                    buf.push(buffer[0]);
                }
            }
        } else if self.is_buf() {
            let result = self.buf().read_until(byte, buf);
            if buf.len() > 0 && buf.last().unwrap() == &byte {
                let _ = buf.pop();
            }
            result
        } else if self.is_piper() {
            let pipe = self.piper();
            loop {
                let l = pipe.read(&mut buffer)?;
                if l == 0 || buffer[0] == byte {
                    return Ok(len);
                } else {
                    len += l;
                    buf.push(buffer[0]);
                }
            }
        } else {
            Ok(0)
        }
    }
    fn new_buf(buf: Box<dyn BufRead>) -> Val {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::Buf(buf));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
    fn new_chars(chars: Box<dyn CharsAPI>) -> Val {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::Chars(chars));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
    fn new_dict() -> Val {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::Dict(Box::new(HashMap::new())));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
}

impl From<isize> for Val {
    #[inline(always)]
    fn from(n: isize) -> Self {
        Val{num: (n<<1) + 1}
    }
}
impl From<f64> for Val {
    #[inline(always)]
    fn from(f: f64) -> Self {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::Float(f));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
}
impl From<RawFd> for Val {
    #[inline(always)]
    fn from(fd: RawFd) -> Self {
        let mut file1 = unsafe { File::from_raw_fd(fd) };
        let mut file2 = file1.try_clone().unwrap();
        std::mem::forget(file1);
        file2.into()
    }
}
impl From<File> for Val {
    fn from(file: File) -> Self {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::File(Box::new(file)));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
}
impl From<PipeReader> for Val {
    fn from(r: PipeReader) -> Self {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::PipeR(Box::new(r)));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
}
impl From<PipeWriter> for Val {
    fn from(w: PipeWriter) -> Self {
        unsafe {
            let result = Val::new();
            let tmp = std::mem::replace(&mut (*result.fat).val, Fat::PipeW(Box::new(w)));
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
}
impl TryFrom<Val> for isize {
    type Error = Val;
    #[inline(always)]
    fn try_from(val: Val) -> Result<Self, Self::Error> {
        unsafe {
            match val.id & TAG_MASK {
                SYM => (&*(*val.var).name).to_string_lossy().parse().or_else(|_| Err(val)),
                CELL|FAT => Err(val),
                VAR => (&*(*val.sym).name).to_string_lossy().parse().or_else(|_| Err(val)),
                _ => {
                    let result = val.num >> 1;
                    std::mem::forget(val);
                    Ok(result)
                }
            }
        }
    }
}
impl TryFrom<Val> for f64 {
    type Error = Val;
    #[inline(always)]
    fn try_from(val: Val) -> Result<Self, Self::Error> {
        unsafe {
            match val.id & TAG_MASK {
                SYM => (&*(*val.var).name).to_string_lossy().parse().or_else(|_| Err(val)),
                CELL => Err(val),
                FAT => {
                    if val.is_float() {
                        match val.fat() {
                            Fat::Float(x) => Ok(*x),
                            _ => panic!()
                        }
                    } else {
                        Err(val)
                    }
                }
                VAR => (&*(*val.sym).name).to_string_lossy().parse().or_else(|_| Err(val)),
                _ => {
                    let result = val.num >> 1;
                    std::mem::forget(val);
                    Ok(result as f64)
                }
            }
        }
    }
}
impl TryFrom<Val> for Cow<'_, Path> {
    type Error = Val;
    #[inline(always)]
    fn try_from(val: Val) -> Result<Self, Self::Error> {
        (&val).try_into().or_else(|_|Err(val))
    }
}
impl TryFrom<Val> for Cow<'_, str> {
    type Error = Val;
    #[inline(always)]
    fn try_from(val: Val) -> Result<Self, Self::Error> {
        (&val).try_into().or_else(|_|Err(val))
    }
}
impl TryFrom<&Val> for Cow<'_, str> {
    type Error = ();
    #[inline(always)]
    fn try_from(val: &Val) -> Result<Self, Self::Error> {
        Ok(unsafe {
            match val.id & TAG_MASK {
                SYM => (*(*val.var).name).to_string_lossy(),
                VAR => (*(*val.sym).name).to_string_lossy(),
                CELL => return Err(()),
                FAT => match val.fat() {
                    Fat::Buf(_)|Fat::Chars(_)|Fat::Dict(_)|Fat::Nothing  => return Err(()),
                    _ => Cow::Owned(format!("{}", val)),
                }
                _ => Cow::Owned(format!("{}", val)),
            }
        })
    }
}
impl TryFrom<&Val> for Cow<'_, Path> {
    type Error = ();
    #[inline(always)]
    fn try_from<'a>(val: &'a Val) -> Result<Self, Self::Error> {
        Ok(unsafe {
            match val.id & TAG_MASK {
                VAR => Cow::Borrowed(&*(*val.var).name),
                SYM => Cow::Borrowed(&*(*val.sym).name),
                CELL => return Err(()),
                FAT => match val.fat() {
                    Fat::Buf(_)|Fat::Chars(_)|Fat::Dict(_)|Fat::Nothing  => return Err(()),
                    _ => Cow::Owned(format!("{}", val).into()),
                }
                _ => Cow::Owned(format!("{}", val).into()),
            }
        })
    }
}
impl TryFrom<&Val> for String {
    type Error = ();
    #[inline(always)]
    fn try_from<'a>(val: &'a Val) -> Result<Self, Self::Error> {
        Cow::<Path>::try_from(val).map(|p|p.to_string_lossy().into_owned())
    }
}
impl PartialEq for Val {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        unsafe {
            self.id == other.id
        }
    }
}

impl<'a> Iterator for &'a Val {
    type Item = &'a Val;
    fn next(&mut self) -> Option<&'a Val> {
        unsafe {
            if self.is_cell() {
                let result = self.car();
                *self = self.cdr();
                Some(result)
            } else {
                None
            }
        }
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        unsafe {
            match self.id & TAG_MASK {
                VAR => {
                    if self.is_str() {
                        write!(f, "{}", (*self.var().name).display())
                    } else {
                        write!(f, "${}", (*self.var().name).display())
                    }
                }
                CELL => {
                    write!(f, "({}", self.car())?;
                    let mut cell = self;
                    loop {
                        cell = cell.cdr();
                        if cell.is_nil() {
                            break;
                        }
                        if !cell.is_cell() {
                            write!(f, " & {}", cell)?;
                            break;
                        }
                        write!(f, " {}", cell.car())?;
                    }
                    write!(f, ")")
                }
                SYM => {
                    write!(f, "{}", (*self.sym().name).display())
                }
                FAT => {
                    let tmp = self.copy().remove_tag(FAT);
                    let result = match tmp.fat() {
                        Fat::Captured(val) => write!(f, "{}", val),
                        Fat::Float(r) => write!(f, "{}", r),
                        Fat::Buf(x) => write!(f, "Buffered"),
                        Fat::Chars(x) => write!(f, "Chars"),
                        Fat::File(x) => write!(f, "{}", x.as_raw_fd()),
                        Fat::PipeR(x) => write!(f, "{}", x.as_raw_fd()),
                        Fat::PipeW(x) => write!(f, "{}", x.as_raw_fd()),
                        Fat::Dict(x) => write!(f, "Dictionary"),
                        Fat::Nothing => write!(f, "Nothing"),
                    };
                    std::mem::forget(tmp);
                    result
                }
                _ => {
                    if self.id & 1 == 1 {
                        write!(f, "{}", (self.num) >> 1)
                    } else {
                        write!(f, "Primitive:{:b}", self.id & !2)
                    }
                }
            }
        }
    }
}

impl fmt::Debug for Val {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt(f)
    }
}

impl Clone for Val {
    #[inline(always)]
    fn clone(&self) -> Val {
        unsafe {
            match self.id &
                TAG_MASK {
                VAR => {
                    if (*self.var).count != 0 {
                        (*self.var).count = (*self.var).count.wrapping_add(1);
                    }
                }
                CELL => {
                    if (*self.cell).count != 0 {
                        (*self.cell).count = (*self.cell).count.wrapping_add(1);
                    }
                }
                FAT => {
                    let tmp = self.copy().remove_tag(FAT);
                    if (*tmp.fat).count != 0 {
                        (*tmp.fat).count = (*tmp.fat).count.wrapping_add(1);
                    }
                    std::mem::forget(tmp);
                }
                _ => {
                }
            }
            Val {id: self.id}
        }
    }
}

impl Drop for Val {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            match self.id & TAG_MASK {
                VAR => {
                    match (*self.var).count {
                        0 => {}
                        1 => {
                            (*self.var).count -= 1;
                            let _ = std::mem::replace(&mut (*self.var).val, Val{sym: ptr::null_mut()});
                            let _ = std::mem::replace(&mut (*self.var).func, Val{sym: ptr::null_mut()});
                            let _ = Box::from_raw(std::mem::replace(&mut (*self.var).name, ptr::null_mut()));
                            NEXT_CELL.with(|next_cell| {
                                (*self.mem).next = next_cell.get();
                                next_cell.set(self.mem);
                            });
                        }
                        _ => {
                            (*self.var).count -= 1;
                        }
                    }
                }
                CELL => {
                    match (*self.cell).count {
                        0 => {}
                        1 => {
                            let _ = std::mem::replace(&mut (*self.cell).car, Val{sym: ptr::null_mut()});
                            let _ = std::mem::replace(&mut (*self.cell).cdr, Val{sym: ptr::null_mut()});
                            NEXT_CELL.with(|next_cell| {
                                let val = Val {id: self.id & !TAG_MASK};
                                let mem = val.mem;
                                (*mem).next = next_cell.get();
                                next_cell.set(mem);
                                std::mem::forget(val);
                            });
                        }
                        _ => {
                            (*self.cell).count -= 1;
                        }
                    }
                }
                FAT => {
                    let tmp = self.copy().remove_tag(FAT);
                    match (*tmp.fat).count {
                        0 => {}
                        1 => {
                            let _ = std::mem::replace(&mut (*tmp.fat).val, Fat::Nothing);
                            NEXT_CELL.with(|next_cell| {
                                let mem = tmp.mem;
                                (*mem).next = next_cell.get();
                                next_cell.set(mem);
                            });
                        }
                        _ => {
                            (*tmp.fat).count -= 1;
                        }
                    }
                    std::mem::forget(tmp);
                }
                _ => {
                }
            }
        }
    }
}

impl std::io::Read for Val {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        if self.is_file() {
            self.file().read(buf)
        } else if self.is_piper() {
            self.piper().read(buf)
        } else if self.is_buf() {
            self.buf().read(buf)
        } else {
            Ok(0)
        }
    }
}

impl std::io::Write for Val {
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        if self.is_file() {
            self.file().write(buf)
        } else if self.is_pipew() {
            self.pipew().write(buf)
        } else {
            Ok(0)
        }
    }
    fn flush(&mut self) -> Result<(), std::io::Error> {
        if self.is_file() {
            self.file().flush()
        } else if self.is_pipew() {
            self.pipew().flush()
        } else {
            Ok(())
        }
    }
}

#[repr(C)]
struct Var {
    val: Val,
    count: usize,
    func: Val,
    name: *mut PathBuf,
}
impl Var {
    #[inline(always)]
    fn eval(&self) -> &Val {
        if self.val.is_captured() {
            self.val.captured()
        } else {
            &self.val
        }
    }
}

#[repr(C)]
struct Cons {
    car: Val,
    count: usize,
    cdr: Val,
}

#[repr(C)]
struct FatPtr {
    count: usize,
    val: Fat,
}

enum Fat {
    Captured(Val),
    Float(f64),
    Buf(Box<dyn BufRead>),
    Chars(Box<dyn CharsAPI>),
    File(Box<File>),
    PipeR(Box<PipeReader>),
    PipeW(Box<PipeWriter>),
    Dict(Box<HashMap<PathBuf, Val>>),
    Nothing,
}

#[repr(C)]
struct Sym {
    func: Val,
    name: *mut PathBuf,
}

#[repr(C)]
#[repr(align(32))]
#[derive (Clone)]
struct Mem {
    next: *mut Mem,
    pad1: usize,
    pad2: usize,
    pad3: usize,
}

#[derive(Debug)]
enum Exception {
    Return,
    ReturnFail,
    Break,
    BreakFail,
    Continue,
    Collect,
    Other,
}

struct Pool {
    next: Option<Box<Pool>>,
    mem: Vec<Mem>,
}
impl Pool {
    fn new() -> Self {
        let size = POOL_SIZE.load(Ordering::SeqCst);
        let mut mem: Vec<Mem> = vec![
            Mem {next: ptr::null_mut(), pad1: 0, pad2: 0, pad3: 0};
            size
        ];
        for i in 0..size - 1 {
            mem[i].next = &mut mem[i + 1] as *mut Mem;
        }

        Self {
            next: None,
            mem: mem,
        }
    }
    fn add_list(mut self) {
        POOL_LIST.with(|pool_list| {
            let old_pool = pool_list.replace(None);
            self.next = old_pool;
            NEXT_CELL.with(|next_cell| {
                next_cell.set(&mut self.mem[0]);
            });
            let _ = pool_list.replace(Some(Box::new(self)));
        });
    }
}

const ZERO: Val = Val {id: 1};
const ONE: Val = Val {id: 3};
struct Env {
    arg_stack: Vec<Val>,
    ver_stack: Vec<Val>,
    rest_stack: Vec<Val>,
    old_rest_stack_len: usize,
    sym: Symbols,
    set_val: Val,
    ret: Val,
    glob_regex: Regex,
    apply_arg: Val,
    jobs: Vec<pid_t>,
    gensym_id: usize,
}
#[derive(Clone)]
struct Symbols {
    nil: Val,
    t: Val,
    env: Val,
    func: Val,
    fn_: Val,
    dynamic: Val,
    var: Val,
    read: Val,
    swap: Val,
    arg: Val,
    argc: Val,
    glob: Val,
    mval: Val,
    quote: Val,
    back_quote: Val,
    stdin: Val,
    stdout: Val,
    stderr: Val,
    ifs: Val,
    type_err: Val,
    arg_err: Val,
    io_err: Val,
    syscall_err: Val,
    regex_err: Val,
    context_err: Val,
    glob_err: Val,
    encode_err: Val,
    parse_err: Val,
    multi_done: Val,
    swap_done: Val,
    progn: Val,
    mac: Val,
    unquote: Val,
    app_arg: Val,
}
impl Env {
    fn new(pool_size: usize, stack_size: usize) -> Env {
        Pool::new().add_list();

        let nil = "()".to_sym(ZERO, ZERO);
        nil.sym().func = nil.clone();
        let _ = NIL.with(|x| x.set(nil.clone()));

        let _ = "if".intern_func(if_);
        let _ = "while".intern_func(while_);
        let _ = "raise".intern_func(raise);
        let _ = "return".intern_func(return_);
        let _ = "break".intern_func(break_);
        let _ = "continue".intern_func(continue_);
        let _ = "catch".intern_func(catch);
        let _ = "shift".intern_func(shift);
        let _ = "spawn".intern_func(spawn);
        let _ = "collect".intern_func(collect);
        let mval = "@".to_sym(nil.clone(), Val{func: mval}.add_tag(FUNC));
        let _ = "gensym".intern_func(gensym);
        let _ = "trap".intern_func(trap);

        let _ = "+".intern_func(calc_fn1::<AddOp>);
        let _ = "-".intern_func(calc_fn1::<SubOp>);
        let _ = "*".intern_func(calc_fn1::<MulOp>);
        let _ = "/".intern_func(calc_fn1::<DivOp>);
        let _ = ">".intern_func(calc_fn2::<Gt>);
        let _ = ">=".intern_func(calc_fn2::<Ge>);
        let _ = "<".intern_func(calc_fn2::<Lt>);
        let _ = "<=".intern_func(calc_fn2::<Le>);
        let _ = "==".intern_func(calc_fn2::<Equal>);
        let _ = "not".intern_func(not);
        let _ = "=".intern_func(same);
        let _ = "is".intern_func(is);
        let _ = "in".intern_func(in_);
        let _ = "~".intern_func(re);
        let _ = "is-list".intern_func(is_list);
        let _ = "is-string".intern_func(is_string);
        let _ = "is-symbol".intern_func(is_symbol);
        let _ = "is-variable".intern_func(is_variable);
        let _ = "is-number".intern_func(is_number);
        let _ = "is-chars".intern_func(is_chars);
        let _ = "is-file".intern_func(is_file);
        let _ = "is-atom".intern_func(is_atom);
        let _ = "is-buffered".intern_func(is_buffered);

        let _ = "cons".intern_func(cons_);
        let _ = "head".intern_func(head);
        let _ = "rest".intern_func(rest);

        let _ = "dict".intern_func(dict);
        let _ = "del".intern_func(del);

        let _ = "split".intern_func(split);
        let _ = "expand".intern_func(expand);
        let _ = "str".intern_func(str);

        let _ = "read-line".intern_func(read_line);
        let _ = "readc".intern_func(read_char);
        let _ = "peekc".intern_func(peek);
        let _ = "cur-line".intern_func(cur_line);
        let _ = "parse".intern_func(parse);
        let _ = "echo".intern_func(echo);
        let _ = "echo".intern_func(show);
        let _ = "echo".intern_func(print);
        let _ = "pipe".intern_func(pipe);
        let _ = "buf".intern_func(buf);
        let _ = "chars".intern_func(chars);
        let _ = "open".intern_func(open);
        let _ = "env-var".intern_func(getenv);

        let std_in :Val = std::io::stdin().as_raw_fd().into();
        let std_out:Val = std::io::stdout().as_raw_fd().into();
        let std_err:Val = std::io::stderr().as_raw_fd().into();

        let sym = Symbols {
            t:   1.into(),
            nil: nil.clone(),

            swap:"swap".intern_func(swap),
            var: "var".intern_func(var),
            func: "func".intern_func(func),
            dynamic: "dynamic".intern(),
            fn_: "fn".intern(),
            mac: "mac".intern(),
            progn: "do".intern_func(progn),
            env: "env".intern_func(env),
            mval,

            read:"read".intern_func(read_line),
            arg: "args".intern_func(arg),
            argc: "argc".intern_func(argc),
            quote: "quote".intern_func(quote),
            back_quote: "back_quote".intern_func(back_quote),
            glob:"glob".intern_func(glob_),
            stdin: "STDIN".intern_and_set(std_in, nil.clone()).remove_tag(SYM),
            stdout:"STDOUT".intern_and_set(std_out, nil.clone()).remove_tag(SYM),
            stderr:"STDERR".intern_and_set(std_err, nil.clone()).remove_tag(SYM),
            ifs:"IFS".intern_and_set(" ".intern(), nil.clone()).remove_tag(SYM),
            type_err:"type-error".intern(),
            arg_err:"argument-error".intern(),
            io_err:"io-error".intern(),
            syscall_err:"systemcall-error".intern(),
            regex_err:"regex-error".intern(),
            context_err:"context-error".intern(),
            glob_err:"glob-error".intern(),
            encode_err:"encode-error".intern(),
            parse_err:"parse-error".intern(),
            multi_done: "multi_done".to_sym(nil.clone(), nil.clone()),
            swap_done: "swap_done".to_sym(nil.clone(), nil.clone()),
            unquote: "unquote".to_sym(nil.clone(), nil.clone()),
            app_arg: cons(cons("@".intern(), cons(cons("args".intern(), nil.clone()), nil.clone())), nil.clone()),
        };

        Self {
            glob_regex: Regex::new(r"(\\\[!?\*)").unwrap(),
            arg_stack: Vec::<Val>::with_capacity(stack_size),
            ver_stack: Vec::<Val>::with_capacity(stack_size),
            rest_stack: Vec::<Val>::with_capacity(stack_size),
            old_rest_stack_len: 0,
            set_val: nil.clone(),
            ret: nil.clone(),
            apply_arg: cons(sym.mval.clone(), cons(sym.arg.clone(), nil.clone())),
            jobs: Vec::<pid_t>::new(),
            gensym_id: 0,
            sym,
        }
    }
    fn nil(&self) -> Val {
        self.sym.nil.clone()
    }
    fn other_err(&mut self, label: Val, msg: String) -> Exception {
        self.push(msg.to_str());
        self.push(label);
        Exception::Other
    }
    fn argument_err(&mut self, name: &str, given: usize, expect: &str) -> Exception {
        self.other_err(self.sym.arg_err.clone(), 
            format!("{}: wrong number of arguments (given {}, expected {})",
            name, given, expect))
    }
    fn type_err(&mut self, name: &str, given: &Val, expect: &str) -> Exception {
        self.type_err_of_type(name, given.type_of(), expect)
    }
    fn type_err_of_type(&mut self, name: &str, given: &str,  expect: &str) -> Exception {
        self.other_err(self.sym.type_err.clone(),
            format!("{}: mismatched types (given {}, expected {})",
            name, given, expect))
    }
    fn type_err_conv(&mut self, name: &str, given: &Val) -> Exception {
        self.other_err(self.sym.type_err.clone(),
            format!("{}: {}: non-numeric string or types", name, given))
    }
    fn type_err_to_str(&mut self, name: &str, given: &Val) -> Exception {
        self.other_err(self.sym.type_err.clone(),
            format!("{}: {}: non-displayable types", name, given.type_of()))
    }
    fn read_err(&mut self, name: &str, e: std::io::Error) -> Exception {
        self.other_err(self.sym.io_err.clone(),
            format!("{}: read error, detail={}", name, e))
    }
    fn regex_err(&mut self, name: &str, given: &str) -> Exception {
        self.other_err(self.sym.regex_err.clone(),
            format!("{}: {}: invalid regular expression", name, given))
    }
    fn encode_err(&mut self, name: &str, given: isize) -> Exception {
        self.other_err(self.sym.encode_err.clone(),
            format!("{}: invalid unicode", given))
    }
    fn push(&mut self, v: Val) {
        self.arg_stack.push(v);
    }
    #[inline(always)]
    fn eval_args(&mut self, ast: &Val) -> Result<usize, Exception> {
        let old_stack_len = self.arg_stack.len();
        let mut ast = ast;
        while ast.is_cell() {
            let _ = self.eval(Mode::None, ast.car())?;
            ast = ast.cdr();
        }
        Ok(old_stack_len)
    }
    fn eval_cmd(&mut self, _: Mode, cmd: &Path, args: &Val) -> Result<bool, Exception> {
        let old_stack_len = self.eval_args(args)?;

        let mut command = Command::new(cmd);
        for _ in old_stack_len..self.arg_stack.len() {
            let v = self.arg_stack.pop().unwrap();
            let s = v.to_path()
                .or_else(|_|Err(self.type_err_to_str("shino", &v)))?;
            command.arg(&*s);
        }

        let std_in = self.sym.stdin.var().val.clone();
        let std_out = self.sym.stdout.var().val.clone();
        let std_err = self.sym.stderr.var().val.clone();

        command.stdin(std_in.to_stdio(self)?)
            .stdout(std_out.to_stdio(self)?)
            .stderr(std_err.to_stdio(self)?).output();
        match command.status() {
            Ok(status) => {
                match status.code() {
                    Some(code) => {
                        self.push((code as isize).into());
                        Ok(code == 0)
                    }
                    None => Err(self.other_err(self.sym.syscall_err.clone(),
                                "unknown error code".to_string()))
                }
            }
            Err(e) => {
                Err(self.other_err(self.sym.syscall_err.clone(),
                    format!("{:?}", e)))
            }
        }
    }
    #[inline(always)]
    fn bind(&mut self, var_idx: usize, arg_idx: usize) -> Result<(), Exception> {
        let var_n = (self.ver_stack.len() - var_idx) / 2;
        let arg_n = self.arg_stack.len() - arg_idx;
        for _ in var_n..arg_n {
            unsafe {
                self.rest_stack.push(self.arg_stack.pop().unwrap_unchecked());
            }
        }
        for i in (0..std::cmp::min(var_n, arg_n)).rev() {
            unsafe {
                let mut val = self.arg_stack.pop().unwrap_unchecked();
                swap_var(self.ver_stack.get_unchecked_mut(var_idx + i * 2), &mut val);
                *self.ver_stack.get_unchecked_mut(var_idx + i * 2 + 1) = val;
            }
        }
        Ok(())
    }
    #[inline(always)]
    fn unbind(&mut self, var_idx: usize) {
        let var_n = (self.ver_stack.len() - var_idx) / 2;
        for _ in 0..var_n {
            unsafe {
                let mut val = self.ver_stack.pop().unwrap_unchecked();
                let mut var = self.ver_stack.pop().unwrap_unchecked();
                swap_var(&var, &mut val);
            }
        }
    }
    #[inline(always)]
    fn stack_to_list(&mut self, mode: Mode, stack_idx: usize) {
        if mode == Mode::Multi {
            self.push(self.sym.multi_done.clone());
        } else {
            let mut list = self.nil();
            for _ in stack_idx..self.arg_stack.len() {
                list = cons(self.arg_stack.pop().unwrap(), list);
            }
            self.push(list);
        }
    }
    #[inline(always)]
    fn leave_last_arg_or_nil(&mut self, stack_idx: usize) {
        if self.arg_stack.len() == stack_idx {
            self.push(self.nil());
        } else if self.arg_stack.len() > stack_idx + 1 {
            let result = self.arg_stack.pop().unwrap();
            self.arg_stack.truncate(stack_idx);
            self.push(result);
        }
    }
    #[inline(always)]
    fn eval_lambda(&mut self, mode: Mode, fenv: &Val, vers: &Val, body: &Val, args: &Val) 
    -> Result<bool, Exception> {
        let old_arg_stack_len = self.arg_stack.len();
        self.old_rest_stack_len = self.rest_stack.len();

        let mut args = args;
        while args.is_cell() {
            if let Err(e) = self.eval(Mode::None, args.car()) {
                match e {
                    Exception::Return|Exception::Break|Exception::Continue => return Ok(true),
                    Exception::ReturnFail|Exception::BreakFail => return Ok(false),
                    _ => return Err(e)
                }
            }
            args = args.cdr();
        }
        let args_len = self.arg_stack.len() - old_arg_stack_len;

        let mut vs = vers;
        let mut vers_len = 0;
        while vs.is_cell() && (vers_len < args_len) {
            unsafe {
                swap_var(vs.car(), self.arg_stack.get_unchecked_mut(old_arg_stack_len + vers_len));
                vers_len += 1;
                vs = vs.cdr();
            }
        }
        while vs.is_cell() {
            let mut val = self.nil();
            swap_var(vs.car(), &mut val);
            self.arg_stack.push(val);
            vs = vs.cdr();
        }

        let rest_len = args_len - vers_len;
        for _ in 0..rest_len {
            unsafe {
                self.rest_stack.push(self.arg_stack.pop().unwrap_unchecked());
            }
        }
        for _ in 0..vers_len {
            unsafe {
                self.ver_stack.push(self.arg_stack.pop().unwrap_unchecked());
            }
        }
        let mut fvs = fenv;
        let mut fenv_len = 0;
        while fvs.is_cell() {
            unsafe {
                let mut val = fvs.car().clone();
                fvs = fvs.cdr();
                swap_var(fvs.car(), &mut val);
                self.ver_stack.push(val);
                fvs = fvs.cdr();
                fenv_len += 1;
            }
        }

        let result = self.eval(mode, body);

        let mut fvs = fenv;
        for i in self.ver_stack.len() - fenv_len..self.ver_stack.len() {
            unsafe {
                fvs = fvs.cdr();
                swap_var(fvs.car(), self.ver_stack.get_unchecked_mut(i));
                fvs = fvs.cdr();
            }
        }
        self.ver_stack.truncate(self.ver_stack.len() - fenv_len);

        let mut vs = vers;
        while vs.is_cell() {
            unsafe {
                let mut val = self.ver_stack.pop().unwrap_unchecked();
                swap_var(vs.car(), &mut val);
                if val.is_num() { std::mem::forget(val); }
                vs = vs.cdr();
            }
        }

        self.rest_stack.truncate(self.old_rest_stack_len);

        result
    }
    #[inline(always)]
    fn app(&mut self, mode: Mode, old_stack_len: usize) -> Result<bool, Exception> {
        if old_stack_len == self.arg_stack.len() {
            self.push(self.nil());
            return Ok(true);
        }
        self.old_rest_stack_len = self.rest_stack.len();
        for _ in old_stack_len + 1..self.arg_stack.len() {
            self.rest_stack.push(self.arg_stack.pop().unwrap());
        }
        let f = self.arg_stack.pop().unwrap();
        self.eval_evaled_cmd(mode, &f, &self.sym.app_arg.clone())
    }
    #[inline(always)]
    fn dict_lookup(&mut self, mode: Mode, d: &mut Box<HashMap<PathBuf, Val>>, arg_len: usize) 
        -> Result<bool, Exception> 
    {
        if arg_len == 0 {
            return Err(self.argument_err("swap", 0, "1 or more"));
        }
        let v = self.arg_stack.pop().unwrap();
        let key = v.to_path()
            .or_else(|_|Err(self.type_err_to_str("shino", &v)))?;
        if arg_len == 1 {
            if mode == Mode::Set {
                let new = std::mem::replace(&mut self.set_val, self.sym.swap_done.clone());
                if let Some(addr) = d.get_mut(&*key) {
                    let old = std::mem::replace(addr, new);
                    self.push(old);
                } else {
                    d.insert(key.to_path_buf(), new);
                    self.push(self.nil());
                }
                Ok(true)
            } else {
                if let Some(val) = d.get(&*key) {
                    self.push(val.clone());
                    Ok(true)
                } else {
                    self.push(self.nil());
                    Ok(false)
                }
            }
        } else {
            if let Some(val) = d.get(&*key) {
                if val.is_dict() {
                    self.dict_lookup(mode, val.dict(), arg_len - 1)
                } else {
                    Err(self.type_err("shino", &val, "dictionary"))
                }
            } else {
                Err(self.type_err_of_type("shino", "()", "dictionary"))
            }
        }
    }
    #[inline(always)]
    fn eval_fat(&mut self, mode: Mode, val: &Val, args: &Val) -> Result<bool, Exception> {
        if val.is_dict() {
            let old_stack_len = self.eval_args(args)?;
            let arg_len = self.arg_stack.len() - old_stack_len;
            for _ in 0..arg_len {
                self.rest_stack.push(self.arg_stack.pop().unwrap());
            }
            self.dict_lookup(mode, val.dict(), arg_len)
        } else {
            Err(self.type_err("shino", val, "evaluable value"))
        }
    }
    #[inline(always)]
    fn eval(&mut self, mode: Mode, ast: &Val) -> Result<bool, Exception> {
        unsafe {
            match ast.id & TAG_MASK {
                VAR => {
                    self.push((*ast.var).eval().clone());
                    Ok(true)
                }
                CELL => self.eval_list(mode, &(*ast.cell).car, &(*ast.cell).cdr),
                _ => {
                    self.push(Val {id: ast.id});
                    Ok(true)
                }
            }
        }
    }
    #[inline(always)]
    fn eval_list(&mut self, mode: Mode, cmd: &Val, args: &Val) -> Result<bool, Exception> {
        unsafe{
            match cmd.id & TAG_MASK {
                // $cmd arg... or 'cmd' arg...
                VAR => self.eval_evaled_cmd(mode, (*cmd.var).eval(), args),
                // (expand ...) arg...
                CELL => {
                    let old_stack_len = self.arg_stack.len();
                    let _ = self.eval(Mode::None, cmd)?;
                    let _ = self.eval_args(args)?;
                    self.app(mode, old_stack_len)
                }
                // sym arg...
                SYM => {
                    let f = &(*cmd.sym).func;
                    if f == &self.sym.nil {
                        self.eval_cmd(mode, &*(*cmd.sym).name, args)
                    } else {
                        self.eval_evaled_cmd(mode, f, args)
                    }
                }
                _ => self.eval_evaled_cmd(mode, cmd, args),
            }
        }
    }
    #[inline(always)]
    fn eval_evaled_cmd(&mut self, mode: Mode, cmd: &Val, args: &Val) -> Result<bool, Exception> {
        unsafe{
            match cmd.id & TAG_MASK {
                // 'cmd' arg...
                VAR => self.eval_cmd(mode, &*(*cmd.var).name, args),
                // (fn ...) arg...
                CELL => self.eval_lambda(mode, &(*cmd.cell).car, &(*((*cmd.cell).cdr.cell)).car,
                                    &(*((*cmd.cell).cdr.cell)).cdr, args),
                SYM => self.eval_list(mode, cmd, args),
                FAT => self.eval_fat(mode, cmd, args),
                // built-in arg...
                _ => {
                    if cmd.id & 1 == 1 {
                        self.eval_cmd(mode, &PathBuf::from((cmd.num>>1).to_string()), args)
                    } else {
                        let tmp = Val {id: cmd.id & !FUNC};
                        let primitive = tmp.func;
                        std::mem::forget(tmp);
                        primitive(self, mode, args)
                    }
                }
            }
        }
    }
    fn macro_expand(&mut self, ast: &Val) -> Option<Val> {
        if ast.is_cell() {
            let old_stack_len = self.arg_stack.len();
            let mut cdr = Some((ast, old_stack_len));
            let mut xs = ast;
            while xs.is_cell() {
                if let Some(v) = self.macro_expand(xs.car()) {
                    self.push(v);
                    cdr = None;
                } else {
                    if cdr.is_none() {
                        cdr = Some((xs, self.arg_stack.len()));
                    }
                    self.push(xs.car().clone());
                }
                xs = xs.cdr();
            }
            let f = &self.arg_stack[old_stack_len];
            if f.is_sym() && f.sym().func.is_cell() && f.sym().func.car() == &self.sym.mac {
                let _ = self.app(Mode::Single, old_stack_len);
                Some(self.arg_stack.pop().unwrap())
            } else {
                let (tmp, l) = cdr.unwrap_or((&self.sym.nil, self.arg_stack.len()));
                self.arg_stack.truncate(l);
                if tmp == ast {
                    None
                } else {
                    let mut result = tmp.clone();
                    for _ in old_stack_len .. l {
                        result = cons(self.arg_stack.pop().unwrap(), result);
                    }
                    Some(result)
                }
            }
        } else {
            None
        }
    }
    fn scope_analyze_rest(&mut self, ast: &Val, def_vars: &mut HashMap<usize, isize>, ref_vars: &mut HashSet<usize>) -> Result<Option<Val>, Exception> {
        Ok(if ast.is_cell() {
            let car = self.scope_analyze(ast.car(), def_vars, ref_vars)?;
            if let Some(cdr) = self.scope_analyze_rest(ast.cdr(), def_vars, ref_vars)? {
                Some(cons(car.unwrap_or_else(||ast.car().clone()), cdr))
            } else if let Some(car) = car {
                Some(cons(car, ast.cdr().clone()))
            } else {
                None
            }
        } else if ast.is_var() {
            ref_vars.insert(unsafe{ast.id} & SYM);
            None
        } else {
            None
        })
    }
    fn scope_analyze(&mut self, ast: &Val, def_vars: &mut HashMap<usize, isize>, ref_vars: &mut HashSet<usize>) -> Result<Option<Val>, Exception> {
        if ast.is_cell() && ast.car() == &self.sym.dynamic || ast.car() == &self.sym.fn_ {
            let name = if ast.car() == &self.sym.dynamic { "let" } else { "fn" };
            let old_stack_len = self.arg_stack.len();

            if !ast.cdr().is_cell() {
                return Err(self.argument_err(name, 0, "1 or more"));
            } else if !ast.cdr().car().is_cell() {
                return Err(self.type_err(name, ast.cdr().car(), "symbol list"));
            }
            let args = ast.cdr().car();
            for i in args {
                if i.is_sym() {
                    *def_vars.entry(unsafe{i.id}).or_insert(0) += 1;
                } else {
                    let name = if ast.car() == &self.sym.var { "let" } else { "fn" }; 
                    return Err(self.type_err(name, i, "symbol"));
                }
            }

            let result = if ast.car() == &self.sym.var {
                Some(cons(self.sym.quote.clone(), cons(self.nil(), 
                            self.scope_analyze_rest(ast, def_vars, ref_vars)?
                            .unwrap_or_else(||ast.clone()))))
            } else {
                let mut ref_vars_next = HashSet::new();
                let result = self.scope_analyze_rest(ast, def_vars, &mut ref_vars_next)?
                    .unwrap_or_else(||ast.clone());

                for i in args {
                    unsafe {
                        *def_vars.get_mut(unsafe{&i.id}).unwrap() *= -1;
                    }
                }
                let mut fenv_arg = self.nil();
                for (i, _) in def_vars.iter().filter(|(&k, &v)| v > 0 && ref_vars_next.contains(&k)) {
                    fenv_arg = cons(Val{id:*i}, fenv_arg);
                }
                for i in args {
                    unsafe {
                        *def_vars.get_mut(unsafe{&i.id}).unwrap() *= -1;
                    }
                }
                ref_vars.extend(ref_vars_next.iter());
                Some(cons(self.sym.quote.clone(), cons(fenv_arg, result)))
            };

            for i in args {
                *def_vars.get_mut(unsafe{&i.id}).unwrap() -= 1;
            }
            Ok(result)
        } else {
            self.scope_analyze_rest(ast, def_vars, ref_vars)
        }
    }

}

trait ToNamedObj {
    fn to_str(self) -> Val;
    fn to_sym(self, val: Val, func: Val) -> Val;
    fn to_var(self) -> Val;
    fn intern(self) -> Val;
    fn intern_and_set(self, val: Val, func: Val) -> Val;
    fn intern_func(self, func: Primitive) -> Val;
}
impl ToNamedObj for PathBuf {
    fn to_str(self) -> Val {
        unsafe {
            let result = Val::new();
            result.copy().init_value_of(&mut (*result.var).val);
            nil().init_value_of(&mut (*result.var).func);
            (*result.var).name = Box::into_raw(Box::new(self));
            (*result.var).count = 1;
            result
        }
    }
    fn to_sym(self, val: Val, func: Val) -> Val {
        unsafe {
            let result = Val::new();
            val.init_value_of(&mut (*result.var).val);
            func.init_value_of(&mut (*result.var).func);
            (*result.var).name = Box::into_raw(Box::new(self));
            (*result.var).count = 0;
            result.add_tag(SYM)
        }
    }
    fn to_var(self) -> Val {
        self.intern().remove_tag(SYM)
    }
    fn intern(self) -> Val  {
        self.intern_and_set(nil(), nil())
    }
    fn intern_and_set(self, val: Val, f: Val) -> Val {
        SYM_TABLE.with(|tab| {
            match tab.borrow().get(&self) {
                Some(sym) => {
                    return sym.clone()
                }
                _ => {
                }
            }
            let sym = self.clone().to_sym(val, f);
            tab.borrow_mut().insert(self, sym.clone());
            sym
        })
    }
    fn intern_func(self, func: Primitive) -> Val {
        let f = Val{func: func}.add_tag(FUNC);
        if self.as_os_str().is_empty() {
            println!("{} = {:b}", self.display(), unsafe{f.id});
        }
        self.intern_and_set(nil().clone(), f)
    }
}
impl ToNamedObj for &str {
    fn to_str(self) -> Val {
        PathBuf::from(self).to_str()
    }
    fn to_sym(self, val: Val, func: Val) -> Val {
        PathBuf::from(self).to_sym(val, func)
    }
    fn to_var(self) -> Val {
        PathBuf::from(self).to_var()
    }
    fn intern(self) -> Val  {
        PathBuf::from(self).intern()
    }
    fn intern_and_set(self, val: Val, f: Val) -> Val {
        PathBuf::from(self).intern_and_set(val, f)
    }
    fn intern_func(self, func: Primitive) -> Val {
        PathBuf::from(self).intern_func(func)
    }
}


static POOL_SIZE: AtomicUsize = AtomicUsize::new(1024);
thread_local!(
    static SYM_TABLE: RefCell<HashMap<PathBuf, Val>> = RefCell::new(HashMap::new());
    static NEXT_CELL: Cell<*mut Mem> = Cell::new(ptr::null_mut());
    static POOL_LIST: RefCell<Option<Box<Pool>>> = RefCell::new(None);
    static NIL: OnceCell<Val> = OnceCell::new();
);
#[inline(always)]
fn nil() -> Val {
    NIL.with(|x|x.get().unwrap().clone())
}

struct PeekableReader<'a, R: std::io::Read> {
    reader: BufReader<R>,
    iter: Peekable<Chars<'a>>,
    buf: String,
    line: usize
}
#[derive(Debug)]
enum ParseErr {
    Read(std::io::Error),
    Syntax(usize, char),
    Other(usize, String),
}

const NAME: &str = "shino";
impl fmt::Display for ParseErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ParseErr::Read(e) => write!(f, "{}: read error, {}", NAME, e),
            ParseErr::Syntax(line, given) =>
                write!(f, "{}: line {}: syntax error near unexpected token `{}'", NAME, line, given),
            ParseErr::Other(line, msg) =>
                write!(f, "{}: line {}: {}", NAME, line, msg),
        }
    }
}
type Parsed<T> = Result<Option<T>, ParseErr>;
impl<'a, R: std::io::Read> PeekableReader<'a, R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader), buf: "".to_string(),
            iter: "".chars().peekable(), line: 1
        }
    }
    fn syntax_err<T>(&mut self) -> Parsed<T> {
        match self.peek() {
            Ok(Some(c)) => Err(ParseErr::Syntax(self.line, c)),
            Ok(None) => Err(ParseErr::Other(self.line, "unexpected EOF".to_string())),
            Err(e) => Err(e)
        }
    }
    fn update(&mut self) -> Parsed<()> {
        self.buf.clear();
        let result = self.reader.read_line(&mut self.buf);
        match result {
            Ok(0) => {
                Ok(None)
            }
            Ok(_) => {
                let chars = unsafe {
                    std::mem::transmute::<Chars<'_>, Chars<'static>>(
                        self.buf.chars()
                    )
                };
                self.iter = chars.peekable();
                Ok(Some(()))
            }
            Err(e) => Err(ParseErr::Read(e))
        }
    }
}
trait CharsAPI {
    fn peek(&mut self) -> Parsed<char>;
    fn next(&mut self) -> Parsed<char>;
    fn skip_if(&mut self, cond: fn(char)->bool);
    fn parse(&mut self, env: &Env) -> Parsed<Val>;
    fn parse_list(&mut self, env: &Env) -> Parsed<Val>;
    fn line(&self) -> usize;
}
impl<'a, R: std::io::Read> CharsAPI for PeekableReader<'a, R> {
    fn line(&self) -> usize {
        self.line
    }
    fn peek(&mut self) -> Parsed<char> {
        match self.iter.peek().cloned() {
            Some(c) => {
                Ok(Some(c))
            }
            _ => {
                match self.update() {
                    Ok(None) => Ok(None),
                    Ok(Some(())) => {
                        self.peek()
                    }
                    Err(e) => Err(e)
                }
            }
        }
    }
    fn next(&mut self) -> Parsed<char> {
        match self.iter.next() {
            Some(c) => {
                if c == '\n' {
                    self.line += 1;
                }
                Ok(Some(c))
            }
            _ => {
                match self.update() {
                    Ok(_) => self.next(),
                    Ok(None) => Ok(None),
                    Err(e) => Err(e)
                }
            }
        }
    }
    fn skip_if(&mut self, cond: fn(char)->bool) {
        loop {
            match self.peek() {
                Ok(Some(c)) if cond(c) => {}
                _ => break
            }
            let _ = self.next();
        }
    }
    fn parse_list(&mut self, env: &Env) -> Parsed<Val> {
        self.skip_if(|c|c.is_ascii_whitespace());
        match self.parse(env)? {
            Some(car) => Ok(Some(cons(car, self.parse_list(env)?.unwrap()))),
            _ => {
                match self.peek()? {
                    Some(c) if c == '&' => {
                        let _ = self.next();
                        self.skip_if(|c|c.is_ascii_whitespace());
                        self.parse(env)
                    }
                    _ => Ok(Some(env.nil()))
                }
            }
        }
    }
    fn parse(&mut self, env: &Env) -> Parsed<Val> {
        let c = match self.peek()? {
            Some(c) => c, _ => return Ok(None)
        };

        match c {
            ';' => {
                self.skip_if(|c|c != '\n');
                self.parse(env)
            }
            '#' => {
                let _ = self.next();
                match self.peek()? {
                    Some(c) if c == '\\' => {
                        let _ = self.next();
                        let Some(c) = self.next()? else {
                            return Err(ParseErr::Other(self.line, "unexpected EOF".to_string()));
                        };
                        let c = match c {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            's' => ' ',
                            _ => c
                        };
                        Ok(Some((c as isize).into()))
                    }
                    Some(c) => Ok(Some((c as isize).into())),
                    _ => Err(ParseErr::Other(self.line, "unexpected EOF".to_string()))
                }
            }
            '$' => {
                let _ = self.next();
                let mut name = "".to_string();
                loop {
                    let Some(c) = self.peek()? else {
                        break;
                    };
                    if name.is_empty() {
                        match c {
                            '@' => {
                                return Ok(Some(cons(env.sym.mval.clone(), 
                                            cons(env.sym.arg.clone(), env.nil()))));
                            }
                            '#'=> {
                                return Ok(Some(cons(env.sym.argc.clone(), env.nil())));
                            }
                            _ => {}
                        }
                    }
                    if c.is_ascii_whitespace() || c.is_ascii_control() {
                        break;
                    }
                    if c.is_ascii_punctuation() {
                        match c {
                            '-'|'_' => {}
                            _ => break
                        }
                    }
                    name.push(c);
                    let _ = self.next();
                }
                if name.is_empty() {
                    return self.parse_list(env);
                }
                if let Ok(Some(c)) = self.peek() {
                    if c == '^' {
                        let _ = self.next();
                    }
                }
                if let Ok(n) = name.parse::<isize>() {
                    return Ok(Some(cons(env.sym.arg.clone(), cons(n.into(), env.nil()))));
                }
                Ok(Some(name.to_var()))
            }
            '\'' => {
                let _ = self.next();
                let mut quoted = "".to_string();
                loop {
                    let Some(c) = self.next()? else {
                        return Err(ParseErr::Other(self.line, "unexpected EOF".to_string()));
                    };
                    if c == '\'' {
                        let Some(c) = self.peek()? else {
                            break;
                        };
                        if c == '\'' {
                            quoted.push(c);
                            let _ = self.next();
                        } else {
                            break;
                        }
                    }
                    quoted.push(c);
                }
                Ok(Some(quoted.to_str()))
            }
            '(' => {
                let _ = self.next();
                let result = self.parse_list(env);
                self.skip_if(|c|c.is_ascii_whitespace());
                match self.peek()? {
                    Some(c) if c == ')' => {
                        let _ = self.next();
                        result
                    }
                    _ => self.syntax_err(),
                }
            }
            '`' => {
                let _ = self.next();
                match self.parse(env)? {
                    Some(val) => Ok(Some(cons(env.sym.quote.clone(), val))),
                    _ => return self.syntax_err()
                }
            }
            '^' => {
                let _ = self.next();
                match self.parse(env)? {
                    Some(val) => Ok(Some(cons(env.sym.back_quote.clone(), val))),
                    _ => return self.syntax_err()
                }
            }
            '~' => {
                let _ = self.next();
                match self.parse(env)? {
                    Some(val) => Ok(Some(cons(env.sym.unquote.clone(), val))),
                    _ => return self.syntax_err()
                }
            }
            '?'|'*' => {
                let _ = self.next();
                Ok(Some(cons(env.sym.glob.clone(), c.to_string().intern())))
            }
            '[' => {
                let _ = self.next();
                let mut glob = "[".to_string();
                loop {
                    let Some(c) = self.next()? else {
                        return Err(ParseErr::Other(self.line, "unexpected EOF".to_string()));
                    };
                    if c == ']' && glob.len() != 1 {
                        glob.push(c);
                        break;
                    }
                    glob.push(c);
                }
                Ok(Some(cons(env.sym.glob.clone(), glob.intern())))
            }
            '@' => {
                let _ = self.next();
                match self.parse(env)? {
                    Some(val) => Ok(Some(cons(env.sym.mval.clone(), cons(val, env.nil())))),
                    _ => Ok(Some("@".intern()))
                }
            }
            ')'|'|'|'&'|'{'|'}'|'>'|'<'|' '|'\t'|'\n'|'\r' => {
                Ok(None)
            }
            _ => {
                let mut name = "".to_string();
                loop {
                    let Some(c) = self.peek()? else {
                        break;
                    };
                    match c {
                        _ if c.is_ascii_whitespace() => break,
                        '#'|'$'|'\''|'('|';'|'`'|'^'|'~'|'?'|'*'|'['|')'|'|'|'&'|'{'|'}'|'>'|'<' => {
                            break;
                        }
                        '\\' => {
                            let _ = self.next();
                            let Some(c) = self.peek()? else {
                                return Err(ParseErr::Other(self.line, "unexpected EOF".to_string()));
                            };
                            match c {
                                'n' => name.push('\n'),
                                'r' => name.push('\r'),
                                't' => name.push('\t'),
                                '\n' => {}
                                '0'..'9' => {
                                    let mut n = c.to_string();
                                    while self.next()?.is_some() {
                                        let Some(c) = self.peek()? else {
                                            break;
                                        };
                                        match c {
                                            '0'..'9' => n.push(c),
                                            _ => break
                                        }
                                    }
                                    let code = u32::from_str_radix(&n, 8).unwrap();
                                    name.push(std::char::from_u32(code as u32).unwrap());
                                    continue;
                                }
                                _ => {
                                    name.push(c);
                                }
                            }
                        }
                        _ => name.push(c)
                    }
                    let _ = self.next();
                }
                if maybe_integer(&name) {
                    if let Ok(n) = name.parse::<isize>() {
                        return Ok(Some(n.into()));
                    }
                }
                Ok(Some(name.intern()))
            }
        }
    }
}

#[inline(always)]
fn maybe_integer(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[0].is_ascii_digit() && bytes[0] != b'0' {
        return true;
    }
    if bytes[0] == b'-' && bytes.len() >= 2 
        && bytes[1].is_ascii_digit() && bytes[1] != b'0' {
        return true;
    }
    if s == "0" {
        return true;
    }
    false
}

#[inline(always)]
fn swap_var(sym: &Val, val: &mut Val) {
    unsafe {
        let var = Val{id: sym.id}.remove_tag(SYM);
        std::mem::swap(&mut (*var.var).val, val);
        std::mem::forget(var);
    }
}

#[inline(always)]
fn cons(car: Val, cdr: Val) -> Val {
    car.cons(cdr)
}

fn swap(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    let mut addr = ast.next().ok_or_else(|| env.argument_err("swap", 0, "2"))?;

    let old_stack_len = env.eval_args(ast)?;
    env.arg_stack.truncate(old_stack_len + 1);

    let val = env.arg_stack.pop().unwrap_or_else(||env.nil());
    if addr.is_var() {
        env.push(if addr.var().val.is_captured() {
            std::mem::replace(addr.var().val.captured(), val)
        } else {
            std::mem::replace(&mut addr.var().val, val)
        });
        Ok(true)
    } else if addr.is_cell() {
        env.set_val = val;
        let result = env.eval(Mode::Set, addr)?;
        if env.set_val == env.sym.swap_done {
            env.set_val = env.nil();
            Ok(true)
        } else {
            env.set_val = env.nil();
            Err(env.type_err("swap", addr, "swappable address"))
        }
    } else {
        Err(env.type_err("swap", addr, "swappable address"))
    }
}

fn func(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() != old_stack_len + 1 {
        return Err(env.argument_err("func", 0, "1"));
    }

    let name = env.arg_stack.pop().unwrap();
    if name.is_sym() {
        env.push(name.sym().func.clone());
        if mode == Mode::Set {
            let new = std::mem::replace(&mut env.set_val, env.sym.swap_done.clone());
            name.sym().func = new;
        }
        Ok(true)
    } else {
        Err(env.type_err("func", &name, "symbol"))
    }
}
fn var(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() != old_stack_len + 1 {
        return Err(env.argument_err("var", 0, "1"));
    }

    let name = env.arg_stack.pop().unwrap();
    if name.is_sym() {
        env.push(name.copy().remove_tag(SYM));
        Ok(true)
    } else {
        Err(env.type_err("var", &name, "symbol"))
    }
}
    
fn if_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    loop { unsafe {
        if !ast.is_cell() {
            let result = env.nil();
            let result = std::mem::replace(&mut env.ret, result);
            env.push(result);
            return Ok(false);
        }
        let car = ast.car();
        ast = ast.cdr();
        if !ast.is_cell() {
            return env.eval(mode.for_special_form(), car);
        }
        let cond = env.eval(Mode::Single, car)?;
        env.ret = env.arg_stack.pop().unwrap();
        if cond {
            return env.eval(mode.for_special_form(), ast.car());
        }
        ast = ast.cdr();
    }}
}
fn progn(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut args = ast;
    if args.is_cell() {
        while args.cdr().is_cell() {
            let _ = env.eval(mode.for_progn(), args.car());
            env.ret = env.arg_stack.pop().unwrap();
            args = args.cdr();
        }
        let result = env.eval(mode.for_special_form(), args.car());
        env.ret = env.nil();
        result
    } else {
        env.push(env.nil());
        Ok(true)
    }
}
fn env(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut vars = ast;
    let mut fenv = env.nil();
    while vars.is_cell() {
        unsafe {
            let var = vars.car().copy().remove_tag(SYM);
            let val = (*var.var).val.clone();
            let captured = val.capture();
            (*var.var).val = captured.clone();
            fenv = cons(captured, cons(var, fenv));
            vars = vars.cdr();
        }
    }
    env.push(fenv);
    Ok(true)
}
fn while_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    let cond = ast.next().ok_or_else(|| env.argument_err("while", 0, "2"))?;
    let body = ast.next().ok_or_else(|| env.argument_err("while", 1, "2"))?;

    let mut result = true; 
    let old_stack_len = env.arg_stack.len();
    let mut stack_len = old_stack_len;
    while env.eval(Mode::Single, cond)? {
        env.ret = env.arg_stack.pop().unwrap();
        match  env.eval(mode.for_progn(), body) {
            Ok(x) => {
                result = x;
                let _ = env.arg_stack.pop().unwrap();
            }
            Err(Exception::Collect) => {
                let collect_old_stack_len = env.arg_stack.pop().unwrap();
                if stack_len != unsafe{ collect_old_stack_len.id >> 1 } {
                    return Err(env.other_err(env.sym.context_err.clone(),
                            "collect: not loop context".to_string()));
                }
                stack_len = env.arg_stack.len();
                std::mem::forget(stack_len);
            }
            e => return e
        }
    }
    env.stack_to_list(mode, old_stack_len);
    Ok(result)
}
fn mval(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    let body = ast.next().ok_or_else(|| env.argument_err("@", 0, "1"))?;
    let result = env.eval(Mode::Multi, body)?;
    let val = env.arg_stack.pop().unwrap();
    if val != env.sym.multi_done {
        if val.is_cell() {
            for v in &val {
                env.push(v.clone());
            }
        } else {
            env.arg_stack.push(val);
        }
    }
    Ok(result)
}
fn raise(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    env.arg_stack.truncate(old_stack_len + 2);
    if env.arg_stack.len() == old_stack_len {
        return Err(env.argument_err("-", 0, "1 or 2"));
    }
    if env.arg_stack.len() == old_stack_len + 1 {
        env.push(env.nil());
    }
    Err(Exception::Other)
}
fn return_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if ast.is_cell() {
        if env.eval(mode.for_return(), ast.car())? {
            Err(Exception::Return)
        } else {
            Err(Exception::ReturnFail)
        }
    } else {
        env.push(env.nil());
        Err(Exception::Return)
    }
}
fn break_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if ast.is_cell() {
        if env.eval(mode.for_return(), ast.car())? {
            Err(Exception::Break)
        } else {
            Err(Exception::BreakFail)
        }
    } else {
        env.push(env.nil());
        Err(Exception::Break)
    }
}
fn continue_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Err(Exception::Continue)
}
fn catch(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_arg_stack_len = env.arg_stack.len();
    let old_var_stack_len = env.ver_stack.len();
    let old_rest_stack_len = env.rest_stack.len();

    let mut ast = ast;
    let body = ast.next().ok_or_else(|| env.argument_err("catch", 0, "2"))?;
    let handler = ast.next().ok_or_else(|| env.argument_err("catch", 1, "2"))?;

    match env.eval(mode, body) {
        Err(Exception::Other) => {
            env.rest_stack.truncate(old_rest_stack_len);
            env.ver_stack.truncate(old_var_stack_len);
            env.rest_stack.push(env.arg_stack.pop().unwrap());
            env.rest_stack.push(env.arg_stack.pop().unwrap());
            env.arg_stack.truncate(old_arg_stack_len);
            let apply_arg = env.apply_arg.clone();
            env.eval_list(mode, &handler, &apply_arg)
        }
        x => x
    }
}
fn shift(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let n: isize = if env.arg_stack.len() == old_stack_len {
        1
    } else {
        env.arg_stack.truncate(old_stack_len + 1);
        env.arg_stack.pop().unwrap().try_into()
            .or_else(|n|Err(env.type_err_conv("shift", &n)))?
    };

    let mut result = env.nil();
    for _ in 0..n {
        if env.rest_stack.len() == env.old_rest_stack_len {
            env.arg_stack.push(env.nil());
            return Ok(false);
        }
        result = env.rest_stack.pop().unwrap();
    }
    env.arg_stack.push(result);
    Ok(true)
}
fn arg(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() == old_stack_len {
        for _ in env.old_rest_stack_len..env.rest_stack.len() {
            env.arg_stack.push(env.rest_stack.pop().unwrap());
        }
        env.stack_to_list(mode, old_stack_len);
    } else {
        env.arg_stack.truncate(old_stack_len + 1);
        let rest_n = env.rest_stack.len() - env.old_rest_stack_len;

        let mut n: isize = env.arg_stack.pop().unwrap().try_into()
            .or_else(|n|Err(env.type_err_conv("arg", &n)))?;
        n -= 1;
        if n < 0 {
            n += rest_n as isize;
        }
        if n < 0 || n as usize >= rest_n {
            env.push(env.nil());
            return Ok(false);
        }

        let l = env.rest_stack.len();
        if mode == Mode::Set {
            let new = std::mem::replace(&mut env.set_val, env.sym.swap_done.clone());
            let old = std::mem::replace(&mut env.rest_stack[l - (n as usize)], new);
            env.push(old);
        } else {
            env.push(env.rest_stack[l - (n as usize)].clone());
        }
    }
    Ok(true)
}
fn argc(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    env.push(Val{id: ((env.rest_stack.len() - env.old_rest_stack_len) << 1) + 1});
    Ok(true)
}
fn spawn(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    unsafe {
        let pid = fork();
        if pid == -1 {
            return Err(env.other_err(env.sym.syscall_err.clone(),
                        "spawn: failed to fork".to_string()));
        } else if pid == 0 {
            if !ast.is_cell() {
                exit(0);
            }
            let old_stack_len = env.arg_stack.len();
            match env.eval(mode.for_special_form(), ast.car()) {
                Ok(true) => exit(0),
                Ok(false) => {
                    env.arg_stack.truncate(old_stack_len + 1);
                    let n = if old_stack_len == env.arg_stack.len() {
                        ONE
                    } else {
                        env.arg_stack.pop().unwrap()
                    };
                    if n.is_num() {
                        exit(isize::try_from(n).unwrap() as i32);
                    } else {
                        exit(1);
                    }
                }
                _ => exit(1)
            }
        } else {
            env.jobs.push(pid);
        }
    }
    Ok(true)
}
fn wait(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len != 1 {
        return Err(env.argument_err("-", env.arg_stack.len() - old_stack_len, "1"));
    }

    let pid: isize = env.arg_stack.pop().unwrap().try_into()
        .or_else(|v|Err(env.type_err_conv("wait", &v)))?;
    let mut status: libc::c_int = 0;
    unsafe {
        let ret = waitpid(pid as pid_t, &mut status as *mut _, 0);
        if ret == -1 {
            return Err(env.other_err(env.sym.syscall_err.clone(),
            format!("wait: failed to wait {}", pid)));
        }
        if WIFEXITED(status) {
            let code = WEXITSTATUS(status);
            env.push(code.into());
            Ok(code == 0)
        } else {
            Err(env.other_err(env.sym.syscall_err.clone(),
            "wait: failed to get error code".to_string()))
        }
    }
}

fn collect(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let result = env.eval(mode, ast.car());
    env.push(Val{id: (old_stack_len << 1) + 1});
    Err(Exception::Collect)
}
fn quote(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    env.push(ast.clone());
    Ok(true)
}
fn back_quote(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if ast.is_cell() {
        let old_stack_len = env.arg_stack.len();
        if ast.car() == &env.sym.unquote {
            let _ = env.eval(Mode::None, ast.cdr())?;
            Ok(true)
        } else {
            let mut unquoted = false;
            for node in ast {
                let unquoted = unquoted || back_quote(env, Mode::None, node)?;
            }
            if unquoted {
                env.stack_to_list(Mode::None, old_stack_len);
            } else {
                env.arg_stack.truncate(old_stack_len);
                env.push(ast.clone());
            }
            Ok(unquoted)
        }
    } else {
        env.push(ast.clone());
        Ok(false)
    }
}

fn gensym(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    env.push(format!("gensym-{}", env.gensym_id).to_sym(env.nil().clone(), env.nil().clone()));
    env.gensym_id += 1;
    Ok(true)
}

fn trap(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}

use std::ops::{Add, Mul, Sub, Div, Neg};
trait Operator<T = Self>
{
    fn apply1(a: T, b: T) -> T;
    fn apply2(a: T, b: T) -> T { Self::apply1(a, b) }
    fn unit(a: T) -> T { a }
    fn id_elem() -> T;
}
trait OperatorInfo {
    fn name() -> &'static str;
}
struct AddOp;
struct MulOp;
struct SubOp;
struct DivOp;
impl<T> Operator<T> for AddOp
where T: Add<Output = T> + From<u8>,
{
    fn apply1(a: T, b: T) -> T { a + b }
    fn id_elem() -> T { 0u8.into() }
}
impl OperatorInfo for AddOp {
    fn name() -> &'static str { "+" }
}
impl<T> Operator<T> for MulOp
where T: Mul<Output = T> + From<u8>,
{
    fn apply1(a: T, b: T) -> T { a * b }
    fn id_elem() -> T { 1u8.into() }
}
impl OperatorInfo for MulOp {
    fn name() -> &'static str { "*" }
}
impl<T> Operator<T> for SubOp
where T: Sub<Output = T> + Add<Output = T> + Neg<Output = T> + From<u8>,
{
    fn apply1(a: T, b: T) -> T { a + b }
    fn apply2(a: T, b: T) -> T { a - b }
    fn unit(a: T) -> T { -a }
    fn id_elem() -> T { 0u8.into() }
}
impl OperatorInfo for SubOp {
    fn name() -> &'static str { "-" }
}
impl<T> Operator<T> for DivOp
where T: Div<Output = T> + Mul<Output = T> + From<u8>,
{
    fn apply1(a: T, b: T) -> T { a * b }
    fn apply2(a: T, b: T) -> T { a / b }
    fn unit(a: T) -> T { T::from(1u8)/a }
    fn id_elem() -> T { 1u8.into() }
}
impl OperatorInfo for DivOp {
    fn name() -> &'static str { "/" }
}
fn fold_fn1<T, Op>(env: &mut Env, stack_len: usize, mut n: T) -> Result<(), T>
where
    T: TryFrom<Val, Error = Val> + Into<Val> + Copy,
    Op: Operator<T>,
{
    while env.arg_stack.len() > stack_len + 1 {
        let v = env.arg_stack.pop().unwrap();
        match T::try_from(v) {
            Ok(m) => {
                n = Op::apply1(n, m);
            }
            Err(v) => {
                env.push(v);
                return Err(n);
            }
        }
    }
    let v = env.arg_stack.pop().unwrap();
    match T::try_from(v) {
        Ok(m) => {
            env.arg_stack.push(Op::apply2(m, n).into());
        }
        Err(v) => {
            env.push(v);
            return Err(n);
        }
    }
    Ok(())
}
fn calc_fn1<Op>(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception>
where
    Op: Operator<isize> + Operator<f64> + OperatorInfo,
{
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len == env.arg_stack.len() {
        let n: isize = Op::id_elem();
        env.push(n.into());
    } else {
        match isize::try_from(env.arg_stack.pop().unwrap()) {
            Ok(n) if old_stack_len + 1 == env.arg_stack.len() => {
                env.push(Op::unit(n).into());
            }
            Ok(n) => {
                match fold_fn1::<isize, Op>(env, old_stack_len, n) {
                    Ok(_) => {},
                    Err(n) => {
                        if fold_fn1::<f64, Op>(env, old_stack_len, n as f64).is_err() {
                            let v = env.arg_stack.pop().unwrap();
                            return Err(env.type_err_conv(Op::name(), &v));
                        }
                    }
                }
            }
            Err(v) => {
                match f64::try_from(v) {
                    Ok(f) if old_stack_len + 1 == env.arg_stack.len() => {
                        env.push(Op::unit(f).into());
                    }
                    Ok(f) => {
                        if fold_fn1::<f64, Op>(env, old_stack_len, f).is_err() {
                            let v = env.arg_stack.pop().unwrap();
                            return Err(env.type_err_conv(Op::name(), &v));
                        }
                    }
                    Err(v) => {
                        return Err(env.type_err_conv(Op::name(), &v));
                    }
                }
            }
        }
    }
    Ok(true)
}
use std::cmp::PartialOrd;
use std::cmp::PartialEq;
trait CmpOperator<T = Self> {
    fn apply1(a: T, b: T) -> bool;
}
struct Lt;
struct Le;
struct Gt;
struct Ge;
struct Equal;
impl<T: PartialOrd> CmpOperator<T> for Lt
{
    fn apply1(a: T, b: T) -> bool { a < b }
}
impl OperatorInfo for Lt {
    fn name() -> &'static str { "<" }
}
impl<T: PartialOrd> CmpOperator<T> for Le
{
    fn apply1(a: T, b: T) -> bool { a <= b }
}
impl OperatorInfo for Le {
    fn name() -> &'static str { "<=" }
}
impl<T: PartialOrd> CmpOperator<T> for Gt
{
    fn apply1(a: T, b: T) -> bool { a > b }
}
impl OperatorInfo for Gt {
    fn name() -> &'static str { ">" }
}
impl<T: PartialOrd> CmpOperator<T> for Ge
{
    fn apply1(a: T, b: T) -> bool { a >= b }
}
impl OperatorInfo for Ge {
    fn name() -> &'static str { ">=" }
}
impl<T: PartialEq> CmpOperator<T> for Equal
{
    fn apply1(a: T, b: T) -> bool { a == b }
}
impl OperatorInfo for Equal {
    fn name() -> &'static str { "==" }
}
fn fold_fn2<T, Op>(env: &mut Env, stack_len: usize, mut n: T) -> Result<bool, T>
where
    T: TryFrom<Val, Error = Val> + Into<Val> + Copy,
    Op: CmpOperator<T> + OperatorInfo,
{
    while env.arg_stack.len() > stack_len {
        let v = env.arg_stack.pop().unwrap();
        match T::try_from(v) {
            Ok(m) => {
                if !Op::apply1(n, m) {
                    if env.arg_stack.len() == stack_len {
                        env.push(m.into());
                    } else {
                        env.arg_stack.truncate(stack_len);
                    }
                    return Ok(false);
                }
                n = m;
            }
            Err(v) => {
                env.push(v);
                return Err(n);
            }
        }
    }
    env.push(n.into());
    Ok(true)
}
fn calc_fn2<Op>(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception>
where
    Op: CmpOperator<isize> + CmpOperator<f64> + OperatorInfo,
{
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    let mut cond = true;
    while ast.is_cell() {
        cond &= env.eval(Mode::None, ast.car())?;
        ast = ast.cdr();
    }
    if !cond {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(false);
    }

    if old_stack_len == env.arg_stack.len() {
        env.push(env.nil());
    } else {
        match isize::try_from(env.arg_stack.pop().unwrap()) {
            Ok(n) if old_stack_len + 1 == env.arg_stack.len() => {
                env.push(n.into());
            }
            Ok(n) => {
                match fold_fn2::<isize, Op>(env, old_stack_len, n) {
                    Err(m) => {
                        match fold_fn2::<f64, Op>(env, old_stack_len, m as f64) {
                            Ok(x) => {
                                env.push(n.into());
                                return Ok(x);
                            }
                            Err(_) => {
                                let v = env.arg_stack.pop().unwrap();
                                return Err(env.type_err_conv(Op::name(), &v));
                            }
                        }
                    }
                    Ok(x) => {
                        env.push(n.into());
                        return Ok(x);
                    }
                }
            }
            Err(v) => {
                match f64::try_from(v) {
                    Ok(f) if old_stack_len + 1 == env.arg_stack.len() => {
                        env.push(f.into());
                    }
                    Ok(f) => {
                        match fold_fn2::<f64, Op>(env, old_stack_len, f) {
                            Ok(x) => {
                                env.push(f.into());
                                return Ok(x);
                            }
                            Err(_) => {
                                let v = env.arg_stack.pop().unwrap();
                                return Err(env.type_err_conv(Op::name(), &v));
                            }
                        }
                    }
                    Err(v) => {
                        return Err(env.type_err_conv(Op::name(), &v));
                    }
                }
            }
        }
    }
    Ok(true)
}
fn same(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    let mut cond = true;
    while ast.is_cell() {
        cond &= env.eval(Mode::None, ast.car())?;
        ast = ast.cdr();
    }
    if !cond {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(false);
    }

    if old_stack_len + 2 > env.arg_stack.len() {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(true);
    }

    let v = env.arg_stack.pop().unwrap();
    let s = v.to_path()
        .or_else(|_|Err(env.type_err_to_str("=", &v)))?;
    let mut result = true;
    for _ in old_stack_len..env.arg_stack.len() {
        let u = env.arg_stack.pop().unwrap();
        match u.to_path() {
            Err(_)  => {
                return Err(env.type_err_to_str("=", &u));
            }
            Ok(t) => {
                if s != t {
                    result = false;
                    break;
                }
            }
        }
    }
    env.push(v);
    Ok(result)
}
fn is(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    let mut cond = true;
    while ast.is_cell() {
        cond &= env.eval(Mode::None, ast.car())?;
        ast = ast.cdr();
    }
    if !cond {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(false);
    }

    if old_stack_len + 2 > env.arg_stack.len() {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(true);
    }

    let v = env.arg_stack.pop().unwrap();
    let mut result = true;
    for _ in old_stack_len..env.arg_stack.len() {
        let u = env.arg_stack.pop().unwrap();
        if v != u {
            result = false;
            break;
        }
    }
    env.arg_stack.truncate(old_stack_len);
    env.push(v);
    Ok(result)
}
fn in_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    let mut cond = true;
    while ast.is_cell() {
        cond &= env.eval(Mode::None, ast.car())?;
        ast = ast.cdr();
    }
    if !cond {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(false);
    }

    if old_stack_len + 2 > env.arg_stack.len() {
        env.leave_last_arg_or_nil(old_stack_len);
        return Ok(true);
    }

    let v = env.arg_stack.pop().unwrap();
    let mut result = true;
    for _ in old_stack_len..env.arg_stack.len() - 1 {
        let u = env.arg_stack.pop().unwrap();
        if u.is_cell() {
            for u in &u {
                if &v != u {
                    result = false;
                    break;
                }
            }
        } else {
            if v != u {
                result = false;
                break;
            }
        }
    }

    env.arg_stack.truncate(old_stack_len);
    env.push(v);
    Ok(result)
}
fn mod_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 2 != env.arg_stack.len() {
        return Err(env.argument_err("%", env.arg_stack.len() - old_stack_len, "2"));
    }
    let n = isize::try_from(env.arg_stack.pop().unwrap());
    let m = isize::try_from(env.arg_stack.pop().unwrap());
    let n = n.or_else(|v|Err(env.type_err("%", &v, "integer")))?;
    let m = m.or_else(|v|Err(env.type_err("%", &v, "integer")))?;
    env.push((n % m).into());

    Ok(true)
}
fn int(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("int", env.arg_stack.len() - old_stack_len, "1"));
    }
    env.arg_stack.truncate(old_stack_len + 1);
    match env.arg_stack[old_stack_len].int() {
        Some(n) => env.arg_stack[old_stack_len] = n.into(),
        None => {
            let v = env.arg_stack[old_stack_len].clone();
            return Err(env.type_err_conv("int", &v));
        }
    }
    Ok(true)
}
fn float(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("float", env.arg_stack.len() - old_stack_len, "1"));
    }
    match f64::try_from(std::mem::replace(&mut env.arg_stack[old_stack_len], ZERO)) {
        Ok(f) => env.arg_stack[old_stack_len] = f.into(),
        Err(v) => {
            env.push(env.arg_stack[old_stack_len].clone());
            return Err(env.type_err_conv("float", &v));
        }
    }
    Ok(true)
}
fn re(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    while ast.is_cell() {
        if !env.eval(Mode::None, ast.car())? {
            env.arg_stack.truncate(old_stack_len);
            return Ok(false);
        }
        ast = ast.cdr();
    }

    if old_stack_len + 2 != env.arg_stack.len() {
        return Err(env.argument_err("%", env.arg_stack.len() - old_stack_len, "2"));
    }

    let v = env.arg_stack.pop().unwrap();
    let u = env.arg_stack.pop().unwrap();

    let s = v.to_str()
        .or_else(|_|Err(env.type_err_to_str("~", &v)))?;
    let re = Regex::new(&s)
        .or_else(|_|Err(env.regex_err("~", &s)))?;

    let t = u.to_str()
        .or_else(|_|Err(env.type_err_to_str("~", &u)))?;
    let result = re.is_match(&t);

    env.push(u);
    Ok(result)
}
fn not(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(!progn(env, mode, ast)?)
}
fn is_list(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_list", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_cell() || env.arg_stack[old_stack_len].is_nil())
}
fn is_atom(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_atom", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(!env.arg_stack[old_stack_len].is_cell())
}
fn is_string(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_string", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_str() || env.arg_stack[old_stack_len].is_sym())
}
fn is_symbol(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_symbol", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_sym())
}
fn is_variable(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_variable", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_var())
}
fn is_number(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_number", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_num() || env.arg_stack[old_stack_len].is_float())
}
fn is_integer(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_integer", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_num())
}
fn is_float(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_float", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_float())
}
fn is_buffered(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_buf", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_buf())
}
fn is_chars(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_chars", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_chars())
}
fn is_file(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("is_file", env.arg_stack.len() - old_stack_len, "1"));
    }
    Ok(env.arg_stack[old_stack_len].is_file())
}

fn cons_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    while old_stack_len + 2 > env.arg_stack.len() {
        env.push(env.nil());
    }

    let mut cdr = env.arg_stack.pop().unwrap();
    if mode == Mode::Multi {
        for v in &cdr {
            env.push(v.clone());
        }
        env.push(env.sym.multi_done.clone());
    } else {
        while env.arg_stack.len() > old_stack_len {
            cdr = cons(env.arg_stack.pop().unwrap(), cdr);
        }
        env.push(cdr);
    }
    Ok(true)
}
fn head(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("head", env.arg_stack.len() - old_stack_len, "1"));
    }
    let v = env.arg_stack.pop().unwrap();
    if v.is_cell() {
        if mode == Mode::Set {
            let new = std::mem::replace(&mut env.set_val, env.sym.swap_done.clone());
            let old = std::mem::replace(v.car_mut(), new);
            env.push(old);
        } else {
            env.push(v.car().clone());
        }
    } else if v.is_nil()  {
        env.push(env.nil());
    } else {
        return Err(env.type_err("head", &v, "list"));
    }
    Ok(true)
}
fn rest(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if old_stack_len + 1 != env.arg_stack.len() {
        return Err(env.argument_err("rest", env.arg_stack.len() - old_stack_len, "1"));
    }
    let v = env.arg_stack.pop().unwrap();
    if v.is_cell() {
        if mode == Mode::Set {
            let new = std::mem::replace(&mut env.set_val, env.sym.swap_done.clone());
            let old = std::mem::replace(v.cdr_mut(), new);
            env.push(old);
        } else {
            env.push(v.cdr().clone());
        }
    } else if v.is_nil()  {
        env.push(env.nil());
    } else {
        return Err(env.type_err("rest", &v, "list"));
    }
    Ok(true)
}

fn dict(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let arg_len = env.arg_stack.len() - old_stack_len;
    if arg_len % 2 != 0 {
        return Err(env.argument_err("dict", arg_len, "even number"));
    }

    for _ in 0..arg_len {
        env.rest_stack.push(env.arg_stack.pop().unwrap());
    }

    let mut result = Val::new_dict();
    for _ in (0..arg_len).step_by(2) {
        let key = Cow::<Path>::try_from(env.rest_stack.pop().unwrap())
            .or_else(|v|Err(env.type_err_to_str("dict", &v)))?;
        let val = env.rest_stack.pop().unwrap();

        result.dict().insert(key.into_owned(), val);
    }
    Ok(true)
}
fn del(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let arg_len = env.arg_stack.len() - old_stack_len;
    if arg_len < 2 {
        return Err(env.argument_err("del", arg_len, "2 or more"));
    }

    let mut dict = std::mem::replace(&mut env.arg_stack[old_stack_len], ZERO);
    if !dict.is_dict() {
        return Err(env.type_err("del", &dict, "Dictionary"));
    }

    for _ in 1..arg_len {
        let key = Cow::<Path>::try_from(env.rest_stack.pop().unwrap())
            .or_else(|v|Err(env.type_err_to_str("del", &v)))?;
        dict.dict().remove(&*key);
    }

    std::mem::swap(&mut env.arg_stack[old_stack_len], &mut dict);
    Ok(true)
}

fn split(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len > 3 {
        return Err(env.argument_err("split", env.arg_stack.len() - old_stack_len, "1~3"));
    }
    while old_stack_len + 3 > env.arg_stack.len() {
        env.push(env.nil());
    }
    let n = match isize::try_from(env.arg_stack.pop().unwrap()) {
        Ok(n) => n as usize,
        Err(v) if v.is_nil() => usize::MAX,
        Err(v) => return Err(env.type_err("split", &v, "integer"))
    };
    let sep = Cow::<str>::try_from(env.arg_stack.pop().unwrap())
        .or_else(|v|Err(env.type_err_to_str("split", &v)))?;
    let s = Cow::<str>::try_from(env.arg_stack.pop().unwrap())
        .or_else(|v|Err(env.type_err_to_str("split", &v)))?;

    let re = Regex::new(&sep)
        .or_else(|_|Err(env.regex_err("split", &sep)))?;

    let mut cdr = env.nil();
    for i in re.splitn(&s, n + 1) {
        env.push(i.to_string().to_str());
    }
    env.stack_to_list(mode, old_stack_len);
    Ok(true)
}
fn flat_list(env: &mut Env, result: &mut Vec<PathBuf>, xs: &Val, globing: bool) -> Result<(), Exception> {
    if xs.is_cell() {
        if xs.car() == &env.sym.glob {
            let s = xs.cdr().to_path()
                .or_else(|_|Err(env.type_err_to_str("expand", xs.cdr())))?;
            result.push(s.to_path_buf());
        } else {
            for i in xs {
                flat_list(env, result, i, globing);
            }
        }
    } else if globing {
        let s = xs.to_str()
            .or_else(|_|Err(env.type_err_to_str("expand", xs)))?;
        result.push(Pattern::escape(&*s).into())
    } else {
        let s = xs.to_path()
            .or_else(|_|Err(env.type_err_to_str("expand", xs)))?;
        result.push(s.to_path_buf());
    }
    Ok(())
}
fn prod(env: &mut Env, left: Vec<PathBuf>, right: Val, globing: bool) -> Result<Vec<PathBuf>, Exception> {
    let mut ss = Vec::<PathBuf>::new();
    flat_list(env, &mut ss, &right, globing)?;

    let mut result = Vec::<PathBuf>::new();
    for i in left {
        for j in &ss {
            result.push(i.join(j));
        }
    }
    Ok(result)
}
fn glob_expand(env: &mut Env, patterns: Vec<PathBuf>) -> Result<Vec<PathBuf>, Exception> {
    let mut result = Vec::<PathBuf>::new();
    for i in patterns {
        let paths = glob(&(i.to_string_lossy())).or_else(|_|Err(env.other_err(env.sym.glob_err.clone(),
                    format!("{}: failed to path name expansion", i.display()))))?;
        for j in paths {
            let path = j.or_else(|e|Err(env.other_err(env.sym.glob_err.clone(),
                            format!("failed to path name expansion: detail={}", e))))?;
            result.push(path);
        }
    }
    Ok(result)
}
fn brace_expand(env: &mut Env, mode: Mode, globing: bool, l: usize) -> Result<bool, Exception> {
    let mut result = vec![PathBuf::new()];
    for _ in 0..l {
        let v = env.rest_stack.pop().unwrap();
        result = prod(env, result, v, globing)?;
    }
    if globing {
        result = glob_expand(env, result)?;
    }
    if mode == Mode::Multi {
        for i in result {
            env.push(i.to_str());
        }
        env.push(env.sym.multi_done.clone());
    } else {
        let mut cdr = nil();
        while let Some(car) = result.pop() {
            cdr = cons(car.to_str(), cdr);
        }
        env.push(cdr);
    }
    Ok(true)
}
fn expand(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let mut globing = false;
    for arg in ast {
        if arg.is_cell() && arg.car() == &env.sym.glob {
            globing == true;
            env.push(arg.clone());
        } else {
            let _ = env.eval(Mode::None, arg)?;
        }
    }
    let mut cell_exist = false;
    let arg_n = env.arg_stack.len() - old_stack_len;
    for _ in 0..arg_n {
        let v = env.arg_stack.pop().unwrap();
        if !cell_exist && v.is_cell() {
            cell_exist = true;
        }
        env.rest_stack.push(v);
    }

    if cell_exist {
        return brace_expand(env, mode, globing, arg_n);
    }

    let mut s = PathBuf::new();
    for _ in 0..arg_n {
        let path = Cow::<Path>::try_from(env.rest_stack.pop().unwrap())
            .or_else(|v|Err(env.type_err_to_str("expand", &v)))?;
        s.push(path);
    }

    env.push(s.to_str());
    Ok(true)
}
fn glob_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let pattern = ast.to_str().unwrap();
    let paths = glob(&*pattern).or_else(|_|Err(env.other_err(env.sym.glob_err.clone(),
                        format!("{}: failed to path name expansion", pattern))))?;
    let old_stack_len = env.arg_stack.len();
    for j in paths {
        let path = j.or_else(|e|Err(env.other_err(env.sym.glob_err.clone(),
                format!("failed to path name expansion: detail={}", e))))?;
        env.push(path.to_str());
    }

    if mode == Mode::None {
        Ok(true)
    } else {
        env.stack_to_list(mode, old_stack_len);
        Ok(true)
    }
}
    
fn str(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let arg_n = env.arg_stack.len() - old_stack_len;
    for _ in 0..arg_n {
        env.rest_stack.push(env.arg_stack.pop().unwrap());
    }
    let mut bytes = Vec::with_capacity(arg_n);
    for _ in 0..arg_n {
        let n: isize = env.rest_stack.pop().unwrap().try_into()
            .or_else(|v|Err(env.type_err_conv("str", &v)))?;
        if n > u32::MAX as isize {
            return Err(env.encode_err("str", n))?;
        }
        let ch = std::char::from_u32(n as u32)
            .ok_or_else(||env.encode_err("str", n))?;
        let mut buf = [0; 4];
        bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
    env.push(PathBuf::from(OsString::from_vec(bytes)).to_str());
    Ok(true)
}

fn read_line(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut buf = vec![];
    let _ = env.sym.stdin.var().val.read_until(b'\n', &mut buf)
        .or_else(|e|Err(env.read_err("read-line", e)));
    env.push(PathBuf::from(OsString::from_vec(buf)).to_str());
    Ok(true)
}
fn readb(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut buf = [0u8; 1];
    match env.sym.stdin.var().val.read(&mut buf) {
        Ok(0) => {
            env.push(env.nil());
            Ok(false)
        }
        Ok(_) => {
            env.push((buf[0] as isize).into());
            Ok(true)
        }
        Err(e) => {
            Err(env.read_err("readb", e))
        }
    }
}
fn read_char(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if !env.sym.stdin.var().val.is_chars() {
        let v = env.sym.stdin.var().val.clone();
        return Err(env.type_err("readc", &v, "chars"));
    }
    match env.sym.stdin.var().val.chars().next() {
        Ok(Some(c)) => {
            env.push((c as isize).into());
            Ok(true)
        }
        Ok(_) => {
            env.push(env.nil());
            Ok(false)
        }
        Err(ParseErr::Read(e)) => {
            Err(env.read_err("readc", e))
        }
        Err(_) => {
            panic!()
        }
    }
}
fn cur_line(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if !env.sym.stdin.var().val.is_chars() {
        let v = env.sym.stdin.var().val.clone();
        return Err(env.type_err("cur-line", &v, "chars"));
    }
    env.push((env.sym.stdin.var().val.chars().line() as isize).into());
    Ok(true)
}
fn peek(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if !env.sym.stdin.var().val.is_chars() {
        let v = env.sym.stdin.var().val.clone();
        return Err(env.type_err("peekc", &v, "chars"));
    }
    match env.sym.stdin.var().val.chars().peek() {
        Ok(Some(c)) => {
            env.push((c as isize).into());
            Ok(true)
        }
        Ok(_) => {
            env.push(env.nil());
            Ok(false)
        }
        Err(ParseErr::Read(e)) => {
            Err(env.read_err("peekc", e))
        }
        Err(_) => {
            panic!()
        }
    }
}
fn parse(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if !env.sym.stdin.var().val.is_chars() {
        let v = env.sym.stdin.var().val.clone();
        return Err(env.type_err("parse", &v, "chars"));
    }
    match env.sym.stdin.var().val.chars().parse(env) {
        Ok(Some(val)) => {
            env.push(val);
            Ok(true)
        }
        Ok(_) => {
            env.push(env.nil());
            Ok(false)
        }
        Err(ParseErr::Read(e)) => {
            Err(env.read_err("peekc", e))
        }
        Err(e) => {
            Err(env.other_err(env.sym.parse_err.clone(),
            format!("parse: {}", e)))
        }
    }
}
fn echo(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let _ = print_internal(env, mode, ast, "echo");
    env.sym.stdin.var().val.write(b"\n");
    Ok(true)
}
fn print(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    print_internal(env, mode, ast, "print")
}
fn print_internal(env: &mut Env, mode: Mode, ast: &Val, name: &str) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let arg_n = env.arg_stack.len() - old_stack_len;
    if arg_n == 0 {
        return Ok(true);
    }
    let tmp = env.sym.ifs.var().val.clone();
    let ifs = tmp.to_path()
        .or_else(|v|Err(env.type_err_to_str(name, &tmp)))?;
    let ifs = ifs.as_os_str().as_encoded_bytes();

    for _ in 0..arg_n {
        env.rest_stack.push(env.arg_stack.pop().unwrap());
    }
    let path = Cow::<Path>::try_from(env.rest_stack.pop().unwrap())
        .or_else(|v|Err(env.type_err_to_str(name, &v)))?;
    env.sym.stdout.var().val.write(path.as_os_str().as_encoded_bytes());
    for _ in 1..arg_n {
        let path = Cow::<Path>::try_from(env.rest_stack.pop().unwrap())
            .or_else(|v|Err(env.type_err_to_str(name, &v)))?;
        env.sym.stdout.var().val.write(ifs);
        env.sym.stdout.var().val.write(path.as_os_str().as_encoded_bytes());
    }
    env.push(env.nil());

    Ok(true)
}
fn show(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let arg_n = env.arg_stack.len() - old_stack_len;
    if arg_n == 0 {
        return Ok(true);
    }
    let tmp = env.sym.ifs.var().val.clone();
    let ifs = tmp.to_path()
        .or_else(|v|Err(env.type_err_to_str("show", &tmp)))?;
    let ifs = ifs.as_os_str().as_encoded_bytes();

    for _ in 0..arg_n {
        env.rest_stack.push(env.arg_stack.pop().unwrap());
    }
    write!(env.sym.stdout.var().val, "{}", env.rest_stack.pop().unwrap());
    for _ in 1..arg_n {
        env.sym.stdout.var().val.write(ifs);
        write!(env.sym.stdout.var().val, "{}", env.rest_stack.pop().unwrap());
    }
    env.push(env.nil());

    Ok(true)
}
    
fn pipe(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    match std::io::pipe() {
        Ok((r, w)) => {
            env.push(r.into());
            env.push(w.into());
            env.stack_to_list(mode,  env.arg_stack.len() - 2);
            Ok(true)
        }
        Err(e) => {
            Err(env.other_err(env.sym.syscall_err.clone(),
            format!("pipe: failed to create pipe: detail={}", e)))
        }
    }
}
fn buf(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len != 1 {
        return Err(env.argument_err("buf", env.arg_stack.len() - old_stack_len, "1"));
    }
    let mut val = env.arg_stack.pop().unwrap();
    env.arg_stack.push(
        if val.is_file() {
            Val::new_buf(Box::new(BufReader::new(val.move_file())))
        } else if val.is_piper() {
            Val::new_buf(Box::new(BufReader::new(val.move_piper())))
        } else if let Ok(s) = val.to_str() {
            Val::new_buf(Box::new(BufReader::new(Cursor::new(s.to_string()))))
        } else {
            return Err(env.type_err("buf", &val, "fd or displayable value"));
        }
    );
    Ok(true)
}
fn chars(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len != 1 {
        return Err(env.argument_err("chars", env.arg_stack.len() - old_stack_len, "1"));
    }
    let mut val = env.arg_stack.pop().unwrap();
    env.arg_stack.push(
        if val.is_file() {
            Val::new_chars(Box::new(PeekableReader::new(BufReader::new(val.move_file()))))
        } else if val.is_piper() {
            Val::new_chars(Box::new(PeekableReader::new(BufReader::new(val.move_piper()))))
        } else if let Ok(s) = val.to_str() {
            Val::new_chars(Box::new(PeekableReader::new(BufReader::new(Cursor::new(s.to_string())))))
        } else {
            return Err(env.type_err("chars", &val, "fd or displayable value"));
        }
    );
    Ok(true)
}
fn open(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len > 1 {
        return Err(env.argument_err("open", env.arg_stack.len() - old_stack_len, "0 or 1"));
    }
    if old_stack_len == env.arg_stack.len() {
        match tempfile() {
            Ok(f) => {
                env.arg_stack.push(f.into());
            }
            Err(e) => {
                return Err(env.other_err(env.sym.syscall_err.clone(),
                format!("open: failed to open tempfile: detail={}", e)));
            }
        }
    } else {
        let val = env.arg_stack.pop().unwrap();
        let path = val.to_path()
            .or_else(|_|Err(env.type_err_to_str("open", &val)))?;
        match OpenOptions::new().read(true).write(true).create(true).open(&path) {
            Ok(f) => {
                env.arg_stack.push(f.into());
            }
            Err(e) => {
                return Err(env.other_err(env.sym.syscall_err.clone(),
                format!("open: failed to open {}: detail={}", path.display(), e)));
            }
        }
    }
    Ok(true)
}
fn macro_expand(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len != 1 {
        return Err(env.argument_err("macro_expand", env.arg_stack.len() - old_stack_len, "1"));
    }
    let v = env.arg_stack.pop().unwrap();
    let result = env.macro_expand(&v).unwrap_or_else(||v);
    let mut def_vars = HashMap::new();
    let mut ref_vars = HashSet::new();
    let result = env.scope_analyze(&result, &mut def_vars, &mut ref_vars)?.unwrap_or_else(||result);
    env.push(result);

    Ok(true)
}
fn eval(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len != 1 {
        return Err(env.argument_err("eval", env.arg_stack.len() - old_stack_len, "1"));
    }
    let v = env.arg_stack.pop().unwrap();
    env.eval(mode, &v)
}
fn fail(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    if ast.is_cell() {
        let _ = env.eval(mode, ast.car())?;
    } else {
        env.push(env.nil());
    }
    Ok(false)
}

fn getenv(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    if env.arg_stack.len() - old_stack_len != 1 {
        return Err(env.argument_err("env-var", env.arg_stack.len() - old_stack_len, "1"));
    }
    let var = env.arg_stack.pop().unwrap();
    let s = var.to_path()
            .or_else(|_|Err(env.type_err_to_str("env-var", &var)))?;
    let result = match env::var(s.as_os_str()) {
        Ok(value) => {
            env.push(value.to_str());
            Ok(true)
        }
        Err(e) => {
            env.push(env.nil());
            Ok(false)
        }
    };
    if mode == Mode::Set {
        let new = std::mem::replace(&mut env.set_val, env.sym.swap_done.clone());
        let t = new.to_path()
            .or_else(|_|Err(env.type_err_to_str("env-var", &env.set_val.clone())))?;
        env::set_var(s.as_os_str(), t.as_os_str());
    }
    result
}

fn main() {
    let mut env = Env::new(1024, 1024);
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "-c" {
            if let Some(code) = args.next() {
                match PeekableReader::new(Cursor::new(code)).parse(&mut env) {
                    Ok(Some(ast)) => {
                        println!("{}", ast);
                        match env.eval(Mode::None, &ast) {
                            Ok(x) => {
                                println!("{}", x);
                                println!("{}", env.arg_stack.pop().unwrap());
                            }
                            Err(Exception::Other) => {
                                println!("{}", env.arg_stack.pop().unwrap());
                                println!("{}", env.arg_stack.pop().unwrap());
                            }
                            Err(e) => println!("{:?}", e)
                        }
                    }
                    Ok(None) => println!("EoF"),
                    Err(e) => println!("{}", e)
                }
            }
        } else {
            match OpenOptions::new().read(true).write(true).create(true).open(&arg) {
                Ok(fd) => {
                    match PeekableReader::new(fd).parse(&mut env) {
                        Ok(Some(ast)) => println!("{}", ast),
                        Ok(None) => println!("EoF"),
                        Err(e) => println!("{}", e)
                    }
                }
                Err(e) => {
                    println!("shino: failed to open {}: detail={}", arg, e);
                }
            }
        }
    }
}
