# shino
# 文法仕様

## 文法

```bnf
spaces = /[ \t]+/ [ #/[^\n]*/ ];
brank =  spaces { /[\n\r]/ [ spaces ] };
digit = /-?[1-9][0-9]*|0/;
symbol = /([^"#$@&`'()^|`{}[];*<>?\\ \t\n\r]|\\.)+/ - digit;
quoted = "'" /([^"]|"")*/ "'";
variable = "$" ( "@" | symbol | digit ) [ "^" ];
block = "(" [ brank ] [ code ] [ brank ] ")";
glob = "*" | "?" | "[" [ "]" ] /([^\]]*|\\.)*/ "]";

value = digit | symbol | quoted | variable | block | glob;

operator =  brank ( "`" symbol "`" | /[&|;<>]+/ ) brank;
value_operator = operator;

expr = value value_operator value { value_operator value };
values = ( value | expr ) { value | expr };
multi_values = "@" values;

word =  multi_values | values;

command  = word { spaces word } | "{" [ brank ] [ code ] [ brank ] "}" | expr;

command_operator = operator | brank;

code = command { command_operator command };
```

## オブジェクトのメモリ表現

オブジェクトのサイズは32byte固定であり、32byte境界にアラインしているものとする

### symbol / variable / string
- **0~7byte**: 即値(数値)又はオブジェクトへのアドレス (stringの場合自分自身のアドレス) 変数に格納された値に対応する
- **8~15byte**: 参照カウント(symbolの場合0固定)
- **16~23byte**: 即値(数値)又はオブジェクトへのアドレス シンボルが示す関数に対応する
- **24~31byte**: 実装言語の文字列オブジェクトへのアドレス

### cell
- **0~7byte**: オブジェクトへのアドレス(最適化用)
- **8~15byte**: 即値(数値)又はオブジェクトへのアドレス(所謂car部)
- **16~23byte**: 即値(数値)又はオブジェクトへのアドレス(所謂cdr部)
- **24~31byte**: 参照カウント

### 参照型 / 実数 / 辞書型 / file / buffered / chars
- **0~7byte**: 型を示すタグ
- **8~15byte**: 参照カウント
- **16~23byte**:
  - **参照型**: 即値(数値)又はオブジェクトへのアドレス
  - **実数**: 64bit 浮動小数点数
  - **辞書型**: 以下のデータを含む実装言語のオブジェクトへの参照
    - 辞書
  - **file**: 以下のデータを含む実装言語のオブジェクトへの参照
    - ファイルハンドル
  - **buffered**: 以下のデータを含む実装言語のオブジェクトへの参照
    - バッファーIO用のオブジェクト
  - **chars**: 以下のデータを含む実装言語のオブジェクトへの参照
    - 次のユニコード文字
    - バッファーIO用のオブジェクト
- **24~31byte**: 予約

## 値の表現

数値およびオブジェクト(のアドレス)は8byteの数値で表現する。

- **数値**: 0から数えて末尾63bit目を1とし、0~62bitで整数を表現する
- **プリミティブ**: 0から数えて末尾62bit目を1とする。末尾62bit目を0とした場合、対応するプリミティブの関数のアドレスと一致する。
- **cell**: cellのアドレスに対し0から数えて60bit目を1とする (8~15byteにアドレス計算なしにアクセスするため)
- **variable / string**: オブジェクトのアドレス
- **symbol**: symbolのアドレスに対し0から数えて59bit目を1とする (16~23byteにアドレス計算なしにアクセスするため)
- **参照型 / 実数 / 辞書型 / buf / chars / file**: オブジェクトのアドレスに対し0から数えて59~60bit目を1とする

## メモリ管理

各オブジェクトは必ずメンバの値を所有する。所有権を共有する際はメモリをインクリメントし、所有権を放棄する際はデクリメントする。デクリメント後、カウンタが0であればオブジェクトをメモリプールに返却する。インクリメント、デクリメントどちらの場合も処理前にカウンタが0であれば処理は行わない。所有権を共有する際にカウンタが0であればインクリメントなしに共有ができる。

## 値の記述形式

以降、便宜上各値を以下のように表現する。

- **数値**: 123, -321など数または#a、#\nなどの文字コード
- **symbol**: abc, @などアルファベットと記号
- **variable**: $abc, $@, $1など$で始まる文字列
- **string**: "abc"などダブルクォーテーションで囲まれた文字列
- **cell**: (1 2 3), (a . b)など丸括弧とドットを用いたS式

## 非終端記号とASTの対応

上記の値によってASTが表現される。非終端記号とASTの対応は以下の通り。

| 非終端記号 | AST(例) |
|------------|---------|
| spaces | N/A |
| brank | N/A |
| digit | 123 |
| symbol | abc |
| quoted | "abc" |
| variable | $abc |
| block | (), (a b c), (do (a b c) (a b c)) |
| glob | (glob . *) |
| value | 123, abc, "abc", $abc, (a b c), (glob . *) |
| operator | +, >=, \|, \|\|, ; |
| value_operator | +, >= |
| expr | (+ 1 2 3) |
| values | 123, (expand 123 abc "abc" $abc (a b) (glob . *)) |
| multi_values | (@ $x) (@ (a b c)) |
| word | 123, (@ $x), (expand 123 abc (+ 1 2 3) (glob . *)) |
| command | (abc 123 (@ $x) (expand 123 abc) (+ 1 2 3)) |
| command_operator | \|, \|\|, ; |
| code | (abc 123), (do (a b c) (a b c)) |

## 評価 (AST基準)

### 値の評価

- **数値**: 0から数えて末尾63bit目を1とし、0~62bitで整数を表現する
- **cell**: cellのアドレスに対し0から数えて60bit目を1とする (8~15byteにアドレス計算なしにアクセスするため)
- **variable / string**: オブジェクトのアドレス
- **symbol**: symbolのアドレスに対し0から数えて59bit目を1とする (16~23byteにアドレス計算なしにアクセスするため)
- **参照型 / 実数 / buf / chars / file**: 値の評価

数値 / プリミティブ / string / symbol / 実数 / 辞書 / buf / chars / file は即値として評価される。variableは格納された値に評価される。variableは格納された値が参照型の場合、参照先の値に評価される。

※参照型はvariableの値としてのみ出現する

### 式の評価

#### 演算子の評価

演算子が値の場合は値として、式の場合は式として評価される。評価後の値が

- **数値 / string の場合**: 対応するパスをコマンド名として外部コマンドが実行される。この時、引数はすべて評価後に文字列に変換される。
- **symbolの場合**: symbolの示す関数または特殊形式が呼び出される。symbolの示す関数がない(nil)場合、名前に対応するパスをコマンド名として外部コマンドが実行される。
- **辞書の場合**: 引数を評価後に文字列に変換した結果をキーとして対応する値に評価する。引数が複数ある場合は以下のASTと等価であるとする。`((dict key1) key2)...`
- **cellの場合**: cellをラムダ式とみなして評価する。この時引数は最初にすべて評価される。
- **プリミティブ**: 対応するプリミティブ(実装言語の関数)を呼び出す。プリミティブが通常の関数の場合、引数は最初にすべて評価される。プリミティブが特殊形式の場合、引数の評価されるかどうか、タイミング回数はプリミティブにより異なる。

### 値の変換

#### 数値への変換
値がsymbol / stringの場合、十進法で解釈して数値への変換を試みる。値が数値でもsymbol / stringでもない場合、または解釈が失敗する場合は例外を上げる。

#### 文字列への変換
値が数値、実数の場合は十進法で解釈してstringへ変換する。fileの場合はfdの数値を文字列にして返す。値がsymbol / stringでも上記でもない場合は例外を上げる。

### 多値の返却

- **演算子が@プリミティブの場合**: 以下の第一引数の戻り値により多値を返却する。
  - **cellの場合**: 線形リストだとみなしてcar部をすべて返却する。例) @( cons 1 ( cons `(1 2) 3 ) ) -> 1 (1 2)
  - **nilの場合**: 値を返却しない
  - **他の場合**: 戻り値をそのまま返す

- **演算子がglobプリミティブの場合**: パターンに一致するパスを多値返却する。パスがゼロ個の場合は例外を投げる。

- **演算子がexpandで引数にglobを含む場合**: パターンに一致するパスを多値返却する。パスがゼロ個の場合は例外を投げる。

多値の展開が許されるのは上記のプリミティブのみであり、上記のプリミティブが特殊形式内で使用された場合でも多値の展開が可能なのはその特殊形式の呼び出し元が@プリミティブにより多値を指定しているときのみ。

例) `(echo (glob *))` が `(echo a.txt b.txt)` と等価な時、
`(echo (do (glob *)))` は `echo `(a.txt b.txt)` と等価であり、
`(echo @(do (glob *)))` は `(echo a.txt b.txt)` と等価。

#### 多値の返却が演算子の評価の場合
演算子の評価において多値が返却された場合、2つめ以降の戻り値は引数だとみなす。

#### 多値の返却が引数の評価の場合
多値を引数のフィールドにすべて展開する。

### ラムダ式の評価

ラムダ式は以下の構造を持つcellである。

```lisp
(
  ( 参照型の値 valiable 参照型の値 valiable ...) /*環境*/
    (symbol ...) /*引数*/
      コマンド ... /*ボディ部*/ 
)
```

※ グローバルなラムダ式は環境がnilになる。また、マクロはmacシンボルが格納される。

ラムダ式の評価は以下の順序で行われる。束縛の方式は動的スコープと同様。

1. 引数の評価
2. 環境内のvaliableに参照型の値を束縛する
3. 引数のsymbolに引数の評価結果を束縛する(引数が足りない場合はnilを束縛) ※symbolに束縛しきれなかった引数は位置パラメータやarg / shiftプリミティブで参照可能
4. ボディ部の評価
5. 束縛したvaliable / symbolの値をリストア
6. ボディ部の最後のコマンドの戻り値で復帰

### 式の呼び出し結果

式は戻り値の他に失敗と成功のステータスを持つ。ifやwhileは条件部の成功を正とみなして動作する。また、論理プリミティブはbool値の代わりに失敗と成功で論理をコーディングする。

### スコープ

ラムダ式A内の自由変数がラムダ式Aより外側のラムダ式の仮引数の場合、自由変数はレキシカルスコープで動作する。それ以外の場合は動的スコープで動作する。

## プリミティブ (AST基準)

### 特殊形式

#### swap
第一引数の値の参照元に第二引数の評価結果を設定し、元の値を返す。

```lisp
(do (swap $a 1) (swap $a 2)) => 1
(do (swap $a (cons 1 2)) (swap (car $a) 3) (head $a)) => 3
```

#### dynamic
ラムダ式を構築する特殊形式の内、環境を作成しないもの。(その場で呼び出されるラムダ式を想定 (= let))

`(dynamic (arg ...) body...)` はmacro-expandの延長でのスコープ解析により `(() (arg ...) body...)` に置換される。

#### fn
ラムダ式を構築する特殊形式の内、環境を作成するもの。

`(fn (arg1 ...) body...)` はmacro-expandの延長でのスコープ解析により `(cons (env 自由変数...) (arg ...) body...)` に置換される。

#### do
引数を順に評価し、最後の結果を返す。最後の引数以外の結果は$?変数に束縛される。

#### if
```lisp
(if [cond then] ... [else])
```

条件分岐。一番左のcond節が成功した場合はthen節を評価し、その結果で返る。失敗した場合は右隣のcond節then節を同様に評価し、どのcond節も失敗した場合はelse節を評価する。else節がない場合は最後のcond節の結果を返す。

`(if cond1 then1 cond2 then2 else)` は `(if cond1 then1 (if cond2 then2 else))` と等価である。

#### while
```lisp
(while cond [body [else]])
```

condが成功する限りbodyを繰り返し評価する。condが失敗した場合elseを評価する。復帰値はnil、ただしcontinueおよびbreakに引数が与えられた場合はその値を蓄積したリストを返す。

- **break**: while ループを抜ける。
- **continue**: while ループの次の繰り返しへ。

#### @
可変長引数展開。`(@ $args)` は $argsの要素を展開。ただし、@はインターンされないため、`@(...)`または`@`(...)`、`@$var`の形でのみ呼び出すことができる。

#### spawn
非同期プロセスの起動。`(spawn (code...))` プロセスIDを返す。

#### quote
引数のリストを評価せずに返す。

#### back-quote, unquote
S式のクォートおよび展開処理。

### その他制御

#### func
シンボルが束縛されている関数オブジェクトを返す。

#### env
引数に指定されたシンボルに束縛された値を参照型に変換、シンボルを変数に変換し、`(参照型 variable 参照型 variable...)` のリストを返す。この時シンボルに参照型をセットする。

#### raise
例外を発生させる。`(raise symbol deatil)`

#### return
関数・マクロからの即時リターン。`(return value)`

#### catch
```lisp
(catch try handler)
```

try部を評価し、例外が上がった場合にhandlerに例外元のraiseの引数を渡して評価する。

#### shift
```lisp
(shift [number])
```

束縛されなかった引数の内number(デフォルト1)番目の引数を返す。束縛されなかった引数のうち、n番目の引数を n - number 番目に変更する。number番目の引数がない場合は失敗する。

#### arg, argc
位置パラメタ$nは`(arg n)`にパースされる。引数が無い場合は束縛されなかった引数を線形リストにして返す。$@は`@(arg)`と等価である。$#は`(argc)`にパースされる。

#### wait
```lisp
(wait [pid])
```

プロセスの終了を待機する。pidがない場合はspawnが生成したすべてのプロセスを待機。

#### gensym
一意なシンボルを生成する。

#### trap
シグナルやエラーに対するハンドラを定義する。

#### eval
S式を評価。

#### macro-expand
マクロ展開の結果を返す。

#### fail
ステータス失敗を返す

### 算術 / 論理

#### 不定個の引数を取るもの

- **+, -, *, /**: 加減乗除
- **==**: 数値と解釈した場合の同値性
- **=**: 文字と解釈した場合の同値性
- **is**: オブジェクトの同一性
- **<, <=, >, >=**: 大小比較

#### 定数個の引数を取るもの

- **not**: 成否を反転する
- **in**: 集合内包含。`(in a (a b c))`
- **~**: 正規表現マッチ

### 型チェック

- **is-list**: /todo/
- **is-empty**: /todo/
- **is-string**: /todo/
- **is-symbol**: /todo/
- **is-variable**: /todo/
- **is-number**: /todo/
- **is-buffered**: /todo/
- **is-chars**: /todo/
- **is-file**: /todo/
- **is-atom**: /todo/

### リスト操作

#### cons
`(cons a b) → (a . b)`

- `(cons)` は `(cons () ())`
- `(cons a)` は `(cons a ())`  
- `(cons a b c)` は `(cons a (cons b c))` と等価

#### head
リストの先頭を返す。

#### rest
リストのcdr部を返す。

### 辞書操作

#### dict
新しい辞書を作成。`(dict key1 val1 key2 val2...)`

#### del
辞書から要素を削除。

### 文字列操作

#### split
```lisp
(split string [regex [count]])
```

区切り文字(正規表現)で分割する。countは最大の分割の上限、regexが省略された場合は文字コードのリストを返す。

#### expand
文字列結合、パス名展開、リストの組み合わせ列挙を行う。

```lisp
(expand a b c) -> 'abc'
(expand a (glob *)) -> a.txt a.img
(expand `(a b) 1 `(c d)) -> (a1c a1d b1c b1d)
```

#### str
文字コードを表す数値を結合した文字列を生成する。

### 入出力

#### read-line
STDINに設定されたオブジェクトがfileかbufferedの場合に1行読み取り。その他の場合はエラー。

#### parse
STDINに設定されたオブジェクトがcharsの場合にS式をパース。その他の場合はエラー。

#### cur-line
STDINに設定されたオブジェクトがcharsの場合に現在の行位置を返す。その他の場合はエラー。

#### peekc
STDINに設定されたオブジェクトがcharsの場合に次の文字を参照し返す。その他の場合はエラー。

#### readb
STDINに設定されたオブジェクトがfileかbufferedの場合に1byte読み取って数値を返す。その他の場合はエラー。

#### readc
STDINに設定されたオブジェクトがcharsの場合に1文字読み取って文字コードを返す。その他の場合はエラー。

#### echo
STDINに設定されたオブジェクトに文字列を出力し、改行する。引数間にはIFSに設定された値を挟み込む。

#### print
STDINに設定されたオブジェクトに文字列を出力する。引数間にはIFSに設定された値を挟み込む。

#### show
STDINに設定されたオブジェクトに文字列を出力する。引数のオブジェクトはすべてデバッグ用に文字列変換される。

#### pipe
無名パイプを生成し、読み取り用fileオブジェクトと書き込み用fileオブジェクトのリストを返す。

#### buf
fileまたは文字列からbufferedを生成して返す。

#### chars
fileまたは文字列からユニコードを前提に一文字ずつとりだすためのcharsオブジェクトを生成する。

#### open
ファイルを開く。引数が無い場合は一時ファイルを開いてfdを返す。

#### env-var
環境変数を参照してstringオブジェクトを生成して返す。

### 特殊なシンボル

- **STDOUT**: 初期値はfd番号1のfileオブジェクト
- **STDIN**: 初期値はfd番号0のfileオブジェクト
- **STDERR**: 初期値はfd番号2のfileオブジェクト
- **IFS**: 初期値はスペースのシンボル

## サンプルコード

```lisp
(def next-token ()
  (swap PEEK-TOKEN (or (read-brank) (read-op) (parse)))

(def read-op ()
    (if (char #;) SEMI-COLON
        (char #() PAREN-L
        (char #)) PAREN-R
        (or (read-back-quoted) (read-operator)))

(def char (c)
  (if (== (peekc) $c) (next-char)))

(def one-of (cs)
  (if (in (peekc) $cs) (next-char)))

(def none-of (cs)
  (if (not (in (peekc) $cs)) (next-char)))

(def read-brank ()
  (if (in (peekc) `(#\s #\t ## #\n))
    (do (while (one-of `(#\s #\t))
          (and (char ##) (while (none-of `(#\n)))
          (if (char #\n)
              (do (read-brank) NEWLINE)
              SPACE))))

(def read-back-quoted ()
  (if (char #`)
      (let (result (intern (str @(while (none-of `(#`)) (collect $?)))))
        (or (char #`) (fatal "need closing '`'"))
        $result)))

(swap OPERATOR-CHARS `(#; #& #| #@ #> #<))
(def read-operator ()
  (if (in (peek) $OPERATOR-CHARS)
      (intern (str @(while (one-of $OPERATOR-CHARS) (collect $?))))))))

(def parse-right (op power)
  (token SPASE)
  (if (token $op)
    (do (token NEWLINE)
         (cons (or (parse $power) (fatal "need right expression for " $op)) (parse-right $op $power))
    $NIL))

(def parse-left (power)
  (token SPASE)
  (if ($PREFIX $PEEK-TOKEN)
    (let ((op power end) $?)
         (next-token)
         (token NEWLINE)
          (let (result (cons $op (parse $power)))
            (end)
            $result))))
    (next-token)

(def token (sym)
  (if (== $PEEK-TOKEN $sym)
    (next-token))

(def fatal ()
  (raise parse-error (expand (cur-line) ": " $@)))

(def parse (power)
  (let (left (parse-left power))
    (token SPASE)
    (while ($INFIX $PEEK-TOKEN)
        (let ((op power) $?)
          (next-token)
          (token NEWLINE)
          (swap left (cons $op (parse-right $PEEK-TOKEN $power))
    $left)))
```
