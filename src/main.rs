use std::fmt;
use std::ptr;
use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions, read};
use std::os::fd::AsRawFd;
use std::io::{BufReader, ErrorKind};
use std::io::{self, Read};
use std::process::{Command, Stdio, exit};
use std::sync::atomic::{AtomicUsize, Ordering};

use regex::Regex;
extern crate libc;
use libc::{fork, waitpid, pid_t};

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
enum Mode {Single, Multi, Set, None}
type Primitive = fn(&mut Env, Mode, &Val) -> Result<bool, Exception>;
const TAG_MASK: usize = 31;
const SYM: usize = 0;
const NUM: usize = 1;
const FUNC: usize = 2;
const CELL: usize = 8;
const VAR: usize = 16;
const FAT: usize = 24;

impl Val {
    #[inline(always)]
    fn to_str<'a>(&'a self) -> Cow<'a, str> {
        unsafe {
            self.try_into().unwrap_unchecked()
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
            match self.id & TAG_MASK {
                NUM => true,
                _ => false
            }
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
    fn is_stream (&self) -> bool {
        unsafe {
            match self.id & TAG_MASK {
                FAT => {
                    let fat = self.copy().remove_tag(FAT);
                    let result = match &(*fat.fat).val {
                        Fat::Stream(_) => true,
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
                let mut tmp = Fat::Captured(self);
                std::mem::swap(&mut tmp, &mut (*result.fat).val);
                std::mem::forget(tmp);
                (*result.fat).count = 1;
                result.add_tag(FAT)
            }
        }
    }
    fn new_file(file: File) -> Val {
        unsafe {
            let result = Val::new();
            let mut tmp = Fat::File(Box::new(file));
            std::mem::swap(&mut tmp, &mut (*result.fat).val);
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
    fn new_stream(strm: Box<dyn StreamAPI>) -> Val {
        unsafe {
            let result = Val::new();
            let mut tmp = Fat::Stream(strm);
            std::mem::swap(&mut tmp, &mut (*result.fat).val);
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
    fn new_dict() -> Val {
        unsafe {
            let result = Val::new();
            let mut tmp = Fat::Dict(Box::new(HashMap::new()));
            std::mem::swap(&mut tmp, &mut (*result.fat).val);
            std::mem::forget(tmp);
            (*result.fat).count = 1;
            result.add_tag(FAT)
        }
    }
    fn type_of(&self) -> &'static str {
        unsafe {
            match self.id & TAG_MASK {
                0 => {
                    if self.var().val.id == self.id {
                        "string"
                    } else {
                        "variable"
                    }
                }
                8 => "list",
                16 => "symbol",
                _ => {
                    if self.id & 1 == 1 {
                        "number"
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
            &mut(*self.cell).car
        }
    }
    #[inline(always)]
    fn cdr(&self) -> &Val {
        if !self.is_cell() {
            panic!();
        }
        unsafe {
            &mut(*self.cell).cdr
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
    fn stream(&mut self) -> &mut dyn StreamAPI {
        if !self.is_stream() {
            panic!();
        }
        match self.fat() {
            Fat::Stream(x) => x.as_mut(),
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
    fn to_stdio(&self, env: &mut Env) -> Result<Stdio, Exception> {
        if self == &env.sym.stdout {
            Ok(Stdio::from(io::stdout()))
        } else if self == &env.sym.stderr {
            Ok(Stdio::from(io::stderr()))
        } else if self == &env.sym.stdin {
            Ok(Stdio::inherit())
        } else if self.is_file() {
            Ok(Stdio::from(self.clone_file()))
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
    fn dict(&self) -> &mut Box<HashMap<String, Val>> {
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
}

impl From<isize> for Val {
    #[inline(always)]
    fn from(n: isize) -> Self {
        Val{num: n<<1 + 1}
    }
}
impl From<f64> for Val {
    #[inline(always)]
    fn from(f: f64) -> Self {
        unsafe {
            let result = Val::new();
            let mut tmp = Fat::Float(f);
            std::mem::swap(&mut tmp, &mut (*result.fat).val);
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
                SYM => (&*(*val.var).name).parse().or_else(|_| Err(val)),
                CELL => Err(val),
                FAT => {
                    if val.is_float() {
                        match val.fat() {
                            Fat::Float(x) => Ok(*x as isize),
                            _ => panic!()
                        }
                    } else {
                        Err(val)
                    }
                }
                VAR => (&*(*val.sym).name).parse().or_else(|_| Err(val)),
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
                SYM => (&*(*val.var).name).parse().or_else(|_| Err(val)),
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
                VAR => (&*(*val.sym).name).parse().or_else(|_| Err(val)),
                _ => {
                    let result = val.num >> 1;
                    std::mem::forget(val);
                    Ok(result as f64)
                }
            }
        }
    }
}
impl TryFrom<&Val> for Cow<'_, str> {
    type Error = Val;
    #[inline(always)]
    fn try_from<'a>(val: &'a Val) -> Result<Self, Self::Error> {
        Ok(unsafe {
            match val.id & TAG_MASK {
                SYM => Cow::Borrowed(&*(*val.var).name),
                VAR => Cow::Borrowed(&*(*val.sym).name),
                _ => Cow::Owned(format!("{}", val)),
            }
        })
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
                        write!(f, "{}", *self.var().name)
                    } else {
                        write!(f, "${}", *self.var().name)
                    }
                }
                CELL => {
                    write!(f, "({}", self.car())?;
                    let mut cell = self;
                    loop {
                        cell = self.cdr();
                        if cell.is_nil() {
                            break;
                        }
                        if !cell.is_cell() {
                            write!(f, " ^ {}", cell)?;
                            break;
                        }
                        write!(f, " {}", cell.car())?;
                    }
                    write!(f, ")")
                }
                SYM => {
                    write!(f, "{}", *self.sym().name)
                }
                FAT => {
                    let tmp = self.copy().remove_tag(FAT);
                    let result = match tmp.fat() {
                        Fat::Captured(val) => write!(f, "{}", val),
                        Fat::Float(r) => write!(f, "{}", r),
                        Fat::Stream(x) => write!(f, "Stream"),
                        Fat::File(x) => write!(f, "{}", x.as_raw_fd()),
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
                            let mut tmp: Val = Val {sym: ptr::null_mut()};
                            std::mem::swap(&mut tmp, &mut (*self.var).val);
                            let mut tmp: Val = Val {sym: ptr::null_mut()};
                            std::mem::swap(&mut tmp, &mut (*self.var).func);
                            let mut tmp: *mut String = ptr::null_mut();
                            std::mem::swap(&mut tmp, &mut (*self.var).name);
                            let _ = Box::from_raw(tmp);
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
                            let mut tmp: Val = Val {sym: ptr::null_mut()};
                            std::mem::swap(&mut tmp, &mut (*self.cell).car);
                            let mut tmp: Val = Val {sym: ptr::null_mut()};
                            std::mem::swap(&mut tmp, &mut (*self.cell).cdr);
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
                            let mut v = Fat::Nothing;
                            std::mem::swap(&mut v, &mut (*tmp.fat).val);
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

#[repr(C)]
struct Var {
    val: Val,
    count: usize,
    func: Val,
    name: *mut String,
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
    Stream(Box<dyn StreamAPI>),
    File(Box<File>),
    Dict(Box<HashMap<String, Val>>),
    Nothing,
}

#[repr(C)]
struct Sym {
    func: Val,
    name: *mut String,
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
    def: Val,
    var: Val,
    read: Val,
    swap: Val,
    arg: Val,
    glob: Val,
    mval: Val,
    stdin: Val,
    stdout: Val,
    stderr: Val,
    type_err: Val,
    arg_err: Val,
    io_err: Val,
    context_err: Val,
    multi_done: Val,
    progn: Val,
    mac: Val,
    unquote: Val,
}
impl Env {
    fn new(pool_size: usize, stack_size: usize) -> Env {
        Pool::new().add_list();

        let nil = "()".to_string().to_sym(ZERO, ZERO);
        nil.sym().func = nil.clone();
        let _ = NIL.with(|x| x.set(nil.clone()));
        let _ = "if".to_string().intern_func(if_);
        let _ = "+".to_string().intern_func(calc_fn1::<AddOp>);

        let sym = Symbols {
            t:   1.into(),
            nil: nil.clone(),
            def: "def".to_string().intern_func(def),
            progn: "do".to_string().intern_func(progn),
            mac: "mac".to_string().intern_func(mac),
            env: "env".to_string().intern_func(env),
            var: "let".to_string().intern_func(var),
            read:"read-line".to_string().intern_func(read_line),
            swap:"set".to_string().intern_func(swap),
            arg: "args".to_string().intern_func(arg),
            mval:"@".to_string().intern_func(mval),
            glob:"glob".to_string().intern(),
            stdin:"stdin".to_string().intern(),
            stdout:"stdout".to_string().intern(),
            stderr:"stderr".to_string().intern(),
            type_err:"type-error".to_string().intern(),
            arg_err:"argument-error".to_string().intern(),
            io_err:"io-error".to_string().intern(),
            context_err:"context-error".to_string().intern(),
            multi_done: "multi_done".to_string().to_sym(nil.clone(), nil.clone()),
            unquote: "~".to_string().to_sym(nil.clone(), nil.clone()),
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
        self.other_err(self.sym.type_err.clone(),
            format!("{}: mismatched types (given {}, expected {})",
            name, given.type_of(), expect))
    }
    fn type_err_conv(&mut self, name: &str, given: &Val) -> Exception {
        self.other_err(self.sym.type_err.clone(),
            format!("{}: {}: non-numeric string", name, given))
    }
    fn push(&mut self, v: Val) {
        self.arg_stack.push(v);
    }
    #[inline(always)]
    fn eval(&mut self, mode: Mode, ast: &Val) -> Result<bool, Exception> {
        unsafe {
            match ast.id & TAG_MASK {
                VAR => {
                    if ast.is_captured() {
                        self.push(ast.captured().clone());
                    } else {
                        self.push((*ast.var).val.clone());
                    }
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
    fn eval_args(&mut self, ast: &Val) -> Result<usize, Exception> {
        let old_stack_len = self.arg_stack.len();
        let mut ast = ast;
        while ast.is_cell() {
            let _ = self.eval(Mode::None, ast.car())?;
            ast = ast.cdr();
        }
        Ok(old_stack_len)
    }
    fn eval_cmd(&mut self, _: Mode, cmd: &str, args: &Val) -> Result<bool, Exception> {
        let old_stack_len = self.eval_args(args)?;

        let mut command = Command::new(cmd);
        for _ in old_stack_len..self.arg_stack.len() {
            let v = self.arg_stack.pop().unwrap();
            command.arg(format!("{}", v));
        }

        command.output();
        match command.status() {
            Ok(status) => {
                match status.code() {
                    Some(code) => {
                        self.push((code as isize).into());
                        Ok(code == 0)
                    }
                    None => Err(self.other_err(self.sym.io_err.clone(),
                                "unknown error code".to_string()))
                }
            }
            Err(e) => {
                Err(self.other_err(self.sym.io_err.clone(),
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
    fn eval_lambda(&mut self, mode: Mode, fenv: &Val, vers: &Val, body: &Val, args: &Val) 
    -> Result<bool, Exception> {
        let old_arg_stack_len = self.arg_stack.len();
        let old_rest_stack_len = self.rest_stack.len();

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

        self.rest_stack.truncate(old_rest_stack_len);

        result
    }
    #[inline(always)]
    fn eval_list(&mut self, mode: Mode, cmd: &Val, args: &Val) -> Result<bool, Exception> {
        unsafe{
            match cmd.id & TAG_MASK {
                VAR => { // $cmd arg... or 'cmd' arg...
                    let cmd = &(*cmd.var).val;
                    match cmd.id & TAG_MASK {
                        0 => self.eval_cmd(mode, &*(*cmd.var).name, args), // 'cmd' arg...
                        8 => self.eval_lambda(mode, &(*cmd.cell).car, &(*((*cmd.cell).cdr.cell)).car,
                        &(*((*cmd.cell).cdr.cell)).cdr, args), // (fn ...) arg...
                        _ => self.eval_list(mode, cmd, args), // sym arg...
                    }
                }
                // { cmd ... }
                CELL => self.eval(mode, &cmd),
                SYM => { // sym arg...
                    let f = &(*cmd.sym).func;

                    match f.id & TAG_MASK {
                        VAR => self.eval_cmd(mode, &*(*f.var).name, args), // fn f () 'cmd'; f arg... ?
                        CELL => self.eval_lambda(mode, &(*f.cell).car, &(*((*f.cell).cdr.cell)).car,
                                                &(*((*f.cell).cdr.cell)).cdr, args),
                        SYM => {
                            if f == &self.sym.nil {
                                self.eval_cmd(mode, &*(*cmd.sym).name, args)
                            } else {
                                self.eval_list(mode, f, args)
                            }
                        }
                        FAT => {
                            Ok(false)
                        }
                        _ => { // built-in arg...
                            let tmp = Val {id: f.id & !2};
                            let primitive = tmp.func;
                            std::mem::forget(tmp);
                            primitive(self, mode, args)
                        }
                    }
                }
                FAT => {
                    Ok(false)
                }
                _ => {
                    match cmd.id & 1 {
                        1 => self.eval_cmd(mode, &(cmd.num>>1).to_string(), args),
                        _ => {
                            let f = Val {id: cmd.id & !FUNC}.func;
                            f(self, mode, args)
                        }
                    }
                }
            }
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
impl ToNamedObj for String {
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
        unsafe {
            let result = Val::new();
            nil().init_value_of(&mut (*result.var).val);
            nil().init_value_of(&mut (*result.var).func);
            (*result.var).name = Box::into_raw(Box::new(self));
            (*result.var).count = 0;
            result
        }
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
        if !self.is_empty() {
            println!("{} = {:b}", self, unsafe{f.id});
        }
        self.intern_and_set(nil().clone(), f)
    }
}

static POOL_SIZE: AtomicUsize = AtomicUsize::new(1024);
thread_local!(
    static SYM_TABLE: RefCell<HashMap<String, Val>> = RefCell::new(HashMap::new());
    static NEXT_CELL: Cell<*mut Mem> = Cell::new(ptr::null_mut());
    static POOL_LIST: RefCell<Option<Box<Pool>>> = RefCell::new(None);
    static NIL: OnceCell<Val> = OnceCell::new();
);
#[inline(always)]
fn nil() -> Val {
    NIL.with(|x|x.get().unwrap().clone())
}

struct Stream<R: std::io::Read> {
    reader: BufReader<R>,
    buf: VecDeque<char>,
    line: usize
}
#[derive(Debug)]
enum ParseErr {
    TokenEnd,
    Eof,
    Read(std::io::Error),
    InvalidUniCode(usize, u8),
    Syntax(usize, char),
    Other(usize, String),
}
const NAME: &str = "valve";
impl fmt::Display for ParseErr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ParseErr::Eof => write!(f, "EOF"),
            ParseErr::TokenEnd => write!(f, "token end"),
            ParseErr::Read(e) => write!(f, "{}: read error, {}", NAME, e),
            ParseErr::InvalidUniCode(line, n) =>
                write!(f, "{}: line {}: invalid unicode character {}", NAME, line, n),
            ParseErr::Syntax(line, given) =>
                write!(f, "{}: line {}: syntax error near unexpected token `{}'", NAME, line, given),
            ParseErr::Other(line, msg) =>
                write!(f, "{}: line {}: {}", NAME, line, msg),
        }
    }
}

trait StreamAPI {
    fn peek(&mut self) -> Result<char, ParseErr>;
    fn any_char(&mut self) -> Result<char, ParseErr>;
    fn restore(&mut self, c: char);
}
impl<R: std::io::Read> StreamAPI for Stream<R> {
    fn peek(&mut self) -> Result<char, ParseErr> {
        if self.buf.is_empty() {
            let mut buf = [0u8; 1];
            loop {
                match self.read_with_block(&mut buf) {
                    Ok(1) => {
                        let c =  char::from_u32(buf[0].into())
                            .ok_or_else(||ParseErr::InvalidUniCode(self.line, buf[0]))?;
                        self.buf.push_back(c);
                        break Ok(c);
                    }
                    Ok(_) => {
                        break Err(ParseErr::Eof);
                    }
                    Err(ref e) if e.kind() == ErrorKind::Interrupted  => {}
                    Err(e)  => {
                        break Err(ParseErr::Read(e));
                    }
                }
            }
        } else {
            unsafe {
                Ok(*self.buf.front().unwrap_unchecked())
            }
        }
    }
    fn any_char(&mut self) -> Result<char, ParseErr> {
        let c = self.peek()?;
        if c == '\n' {
            self.line += 1;
        }
        unsafe {
            self.buf.pop_front().unwrap_unchecked();
            Ok(c)
        }
    }
    fn restore(&mut self, c: char) {
        self.buf.push_front(c);
    }
}

impl<R: std::io::Read> Stream<R> {
    fn new(reader: BufReader<R>) -> Self {
        Self {reader, buf: VecDeque::new(), line: 1}
    }
    fn read_with_block(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match self.reader.read(buf) {
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                self.reader.get_mut().read(buf)
            }
            x => x
        }
    }
    fn char_of(&mut self, c: char) -> Result<char, ParseErr> {
        if c == self.peek()? {
            self.any_char()
        } else {
            Err(ParseErr::TokenEnd)
        }
    }
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

fn swap_one(env: &mut Env, i: usize, addr: &mut Val) -> Result<bool, Exception> {
    let mut default = env.nil();
    if addr.is_sym() {
        let addr = addr.copy().remove_tag(SYM);
        if addr.var().val.is_captured() {
            std::mem::swap(env.arg_stack.get_mut(i).unwrap_or(&mut default),
                            &mut addr.var().val.captured());
        } else {
            std::mem::swap(env.arg_stack.get_mut(i).unwrap_or(&mut default),
                            &mut addr.var().val);
        }
    } else {
        std::mem::swap(env.arg_stack.get_mut(i).unwrap_or(&mut default),
                        &mut env.set_val);
        let _ = env.eval(Mode::Set, addr)?;
        std::mem::swap(env.arg_stack.get_mut(i).unwrap_or(&mut default),
                        &mut env.set_val);
    }
    if i >= env.arg_stack.len() {
        env.push(default);
    }
    Ok(true)
}

fn swap(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    let mut addrs = ast.next().ok_or_else(|| env.argument_err("swap", 0, "2"))?;
    let cmd = ast.next().ok_or_else(|| env.argument_err("swap", 1, "2"))?;

    let m = if addrs.is_cell() && addrs.cdr().is_cell() {
        Mode::Multi
    } else {
        Mode::Single
    };
    let old_stack_len = env.arg_stack.len();
    let mut i = old_stack_len;
    let result = env.eval(m, cmd);

    while addrs.is_cell() {
        swap_one(env, i, &mut addrs.car_mut());
        i += 1;
        addrs = addrs.cdr();
    }
    env.stack_to_list(mode, old_stack_len);
    result
}

fn var_bind(env: &mut Env, binds: &Val) -> Result<(), Exception> {
    if binds.is_cell() && binds.cdr().is_cell() {
        let vars = binds.car();
        let old_var_stack_len = env.ver_stack.len();
        let old_stack_len = env.arg_stack.len();

        let mode = if vars.is_cell() {
            for var in vars {
                if !vars.is_sym() {
                    return Err(env.type_err("let", vars, "symbol"));
                }
                env.ver_stack.push(var.clone());
                env.ver_stack.push(env.nil());
            }
            Mode::Multi
        } else if vars.is_sym() {
            env.ver_stack.push(vars.clone());
            env.ver_stack.push(env.nil());
            Mode::Single
        } else {
            return Err(env.type_err("let", vars, "symbol"));
        };

        let _ = env.eval(mode, binds.cdr().car());
        let _ = var_bind(env, binds.cdr().cdr())?;

        let _ = env.bind(old_var_stack_len, old_stack_len)?;
    }
    Ok(())
}
fn var(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    let binds = ast.next().ok_or_else(|| env.argument_err("let", 0, "2"))?;
    let body = ast.next().ok_or_else(|| env.argument_err("let", 1, "2"))?;

    let old_var_stack_len = env.ver_stack.len();
    let _ = var_bind(env, &binds)?;
    let result = env.eval(mode, &body);
    env.unbind(old_var_stack_len);
    result
}
fn def_internal(env: &mut Env, ast: &Val, fenv: Val) -> Result<bool, Exception> {
    let mut ast = ast;
    let name = ast.next().ok_or_else(|| env.argument_err("fn", 0, "3 or more"))?;
    let args = ast.next().ok_or_else(|| env.argument_err("fn", 1, "3 or more"))?;
    if !ast.is_cell() {
        return Err(env.argument_err("fn", 2, "3 or more"));
    }
    let body = if ast.cdr().is_cell() {
        cons(env.sym.progn.clone(), ast.clone())
    } else {
        ast.car().clone()
    };

    name.sym().func = cons(fenv, cons(args.clone(), cons(body, env.nil())));
    Ok(true)
}
fn def(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    def_internal(env, ast, env.nil())
}
fn mac(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    def_internal(env, ast, env.sym.mac.clone())
}
fn if_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut ast = ast;
    loop { unsafe {
        if !ast.is_cell() {
            env.push(env.nil());
            return Ok(true);
        }
        let car = ast.car();
        ast = ast.cdr();
        if !ast.is_cell() {
            return env.eval(mode, car);
        }
        let cond = env.eval(Mode::Single, car)?;
        env.ret = env.arg_stack.pop().unwrap();
        if cond {
            return env.eval(mode, ast.car());
        }
        ast = ast.cdr();
    }}
}
fn progn(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut args = ast;
    let mut result = Ok(true);
    if args.is_cell() {
        let result = env.eval(Mode::Single, args.car());
        args = args.cdr();
    }
    while args.is_cell() {
        env.ret = env.arg_stack.pop().unwrap();
        let result = env.eval(Mode::Single, args.car());
        args = args.cdr();
    }
    result
}
fn env(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let mut vers = ast;
    let mut fenv = env.nil();
    while vers.is_cell() {
        unsafe {
            let var = vers.car().copy().remove_tag(SYM);
            let val = (*var.var).val.clone();
            let captured = val.capture();
            (*var.var).val = captured.clone();
            fenv = cons(captured, cons(var, fenv));
            vers = vers.cdr();
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
        match  env.eval(Mode::Single, cond) {
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
    let result = env.eval(Mode::Multi, ast)?;
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

        env.push(env.rest_stack[env.rest_stack.len() - (n as usize)].clone());
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
            return Err(env.other_err(env.sym.io_err.clone(),
                        "spawn: failed to fork".to_string()));
        } else if pid == 0 {
            let old_arg_stack_len = env.arg_stack.len();
            match env.eval(Mode::Multi, ast) {
                Ok(true) => exit(0),
                Ok(false) => {
                    env.arg_stack.truncate(old_arg_stack_len + 1);
                    let n = if old_arg_stack_len == env.arg_stack.len() {
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
fn collect(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let result = env.eval(mode, ast.car());
    env.push(Val{id: old_stack_len << 1 + 1});
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
                    env.arg_stack.truncate(stack_len);
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
    Ok(true)
}
fn calc_fn2<Op>(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception>
where
    Op: CmpOperator<isize> + CmpOperator<f64> + OperatorInfo,
{
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    while ast.is_cell() {
        if !env.eval(Mode::None, ast.car())? {
            env.arg_stack.truncate(old_stack_len);
            return Ok(false);
        }
        ast = ast.cdr();
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
    while ast.is_cell() {
        if !env.eval(Mode::None, ast.car())? {
            env.arg_stack.truncate(old_stack_len);
            return Ok(false);
        }
        ast = ast.cdr();
    }

    if old_stack_len == env.arg_stack.len() {
        env.push(env.nil());
        return Ok(true);
    } else if old_stack_len + 1 == env.arg_stack.len() {
        return Ok(true);
    }

    let v = env.arg_stack.pop().unwrap();
    match unsafe{v.id} & TAG_MASK {
        CELL|FAT if !v.is_float()  => {
          return Err(env.type_err("=", &v, "symbol or string or number"));
        }
        _ => {}
    }
    let s = v.to_str();
    for _ in old_stack_len..env.arg_stack.len() - 1 {
       if s != env.arg_stack.pop().unwrap().to_str() {
           env.arg_stack.truncate(old_stack_len);
           env.push(v);
           return Ok(false);
       }
    }
    env.push(v);

    Ok(true)
}
fn is(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.arg_stack.len();
    let mut ast = ast;
    while ast.is_cell() {
        if !env.eval(Mode::None, ast.car())? {
            env.arg_stack.truncate(old_stack_len);
            return Ok(false);
        }
        ast = ast.cdr();
    }

    if old_stack_len == env.arg_stack.len() {
        env.push(env.nil());
        return Ok(true);
    } else if old_stack_len + 1 == env.arg_stack.len() {
        return Ok(true);
    }

    let v = env.arg_stack.pop().unwrap();
    for _ in old_stack_len..env.arg_stack.len() - 1 {
       if v != env.arg_stack.pop().unwrap() {
           env.arg_stack.truncate(old_stack_len);
           env.push(v);
           return Ok(false);
       }
    }
    env.push(v);

    Ok(true)
}
fn mod_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn int(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn float(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn re(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn not(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_list(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_empty(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_string(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_symbol(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_variable(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_number(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_stream(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn is_file(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}

fn cons_(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn head(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn rest(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
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
        let key = env.rest_stack.pop().unwrap();
        let val = env.rest_stack.pop().unwrap();
        result.dict().insert(key.to_str().into_owned(), val);
    }
    Ok(true)
}
fn del(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    let arg_len = env.arg_stack.len() - old_stack_len;
    if arg_len < 2 {
        return Err(env.argument_err("del", arg_len, "2 or more"));
    }

    let mut dict = env.nil();
    std::mem::swap(&mut dict, &mut env.arg_stack[old_stack_len]);
    if !dict.is_dict() {
        return Err(env.type_err("del", &dict, "Dictionary"));
    }

    for _ in 1..arg_len {
        dict.dict().remove(&*env.arg_stack.pop().unwrap().to_str());
    }

    let _ = env.arg_stack.pop();
    Ok(true)
}

fn split(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn expand(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn num_to_str(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}

fn read_line(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn read_char(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn peek_char(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn read_atom(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn echo(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn printf(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn pipe(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn buf(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    for i in old_stack_len - 1..env.arg_stack.len() {
        let mut val = env.nil();
        std::mem::swap(&mut val, &mut env.arg_stack[i]);
        if val.is_file() {
            env.arg_stack[i] = Val::new_stream(Box::new(Stream::new(BufReader::new(val.move_file()))));
        } else if val == env.sym.stdin {
            env.arg_stack[i] = Val::new_stream(Box::new(Stream::new(BufReader::new(io::stdin()))));
        } else {
            return Err(env.type_err("buf", &val, "fd"));
        }
    }
    Ok(true)
}
fn open(env: &mut Env, _: Mode, ast: &Val) -> Result<bool, Exception> {
    let old_stack_len = env.eval_args(ast)?;
    for i in old_stack_len - 1..env.arg_stack.len() {
        let path = env.arg_stack[i].to_str().into_owned();
        match OpenOptions::new().read(true).write(true).create(true).open(&path) {
            Ok(f) => {
                env.arg_stack[i] = Val::new_file(f);
            }
            Err(e) => {
                return Err(env.other_err(env.sym.io_err.clone(),
                format!("open: failed to open {}: error code: {}", path, e)));
            }
        }
    }
    Ok(true)
}
fn load(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn setenv(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}
fn getenv(env: &mut Env, mode: Mode, ast: &Val) -> Result<bool, Exception> {
    Ok(true)
}

            

                    
                
                    


fn main() {


}
