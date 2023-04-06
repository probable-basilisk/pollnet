(def twitch-chan (in (dyn :args) 1))

## This is terminal coloring with escape codes
(def term/CSI (string (string/from-bytes 27) "["))

(defn term/sgr [& args]
  (string term/CSI (string/join (map string args) ";") "m"))

(def term/RESET (term/sgr 0))

(defn term/rgb [rgb & msg]
  (string (term/sgr 38 2 ;rgb) (string/join msg) term/RESET))

(defn x-byte [v b] (band 0xff (brshift v (* 8 b))))
(defn pastel [v b] (+ 127 (% 128 (x-byte v b))))

(defn hash-color [name]
  (def v (math/abs (hash name)))
  @[(pastel v 0) (pastel v 1) (pastel v 2)])

## This is just IRC message parsing, you can skip to later
## to see how pollnet specifically works
(defn until [patt] ~(any (if-not ,patt 1)))
(defn until+ [patt] ~(* (any (if-not ,patt 1)) ,patt))
(defn until? [patt] ~(* (any (if-not ,patt 1)) (opt ,patt)))

(def irc-patt (peg/compile
  ~(* (<- (any :S)) :s (<- (any :S)) :s (<- (any 1)))
  ))

(def body-patt (peg/compile 
  ~(* ,(until? ":") (<- (any 1)))
  ))

(def ident-patt (peg/compile ~(* (opt ":") (<- ,(until "!")))))

(defn parse-ident [ident]
  (def [uname] (peg/match ident-patt ident))
  uname)

(defn parse-body [msg]
  (def [body] (peg/match body-patt msg))
  body)

(defn parse-line [msg]
  (def [ident cmd rem] (peg/match irc-patt msg))
  (if (= cmd "PRIVMSG") 
    [(parse-ident ident) (parse-body rem)] 
    [(string "SYS > " cmd) rem]))

(defn split-irc [msg] 
  (string/split "\n" (string/replace-all "\r" "" (string/trim msg))))

(defn parse-irc [msg]
  (map parse-line (split-irc msg)))

(defn print-line [msg]
  (def [uname rem] msg)
  (print (term/rgb (hash-color uname) "[" uname "]: ") rem))

(defn print-irc-msg [msg]
  (map print-line (parse-irc msg)))

## Actual networking stuff starts here

(def is-windows (= :windows (os/which)))
(def pollnet-loc 
  (string 
    (if is-windows "" "lib")
    "pollnet" 
    (if is-windows ".dll" ".so")))
# Enable unicode+colors in windows terminal
(if is-windows (os/shell "chcp 65001"))

(ffi/context pollnet-loc)

(ffi/defbind pollnet-init :ptr [])
(ffi/defbind pollnet-shutdown :void [ctx :ptr])
(ffi/defbind pollnet-open-ws :u64 [ctx :ptr url :string])
(ffi/defbind pollnet-send :void [ctx :ptr sock :u64 data :string])
(ffi/defbind pollnet-update :u32 [ctx :ptr sock :u64])
(ffi/defbind pollnet-get-data-size :u32 [ctx :ptr sock :u64])
(ffi/defbind pollnet-unsafe-get-data-ptr :ptr [ctx :ptr sock :u64])
(ffi/defbind pollnet-close :void [ctx :ptr sock :u64])

(def gctx (pollnet-init))

(defn copy-cdata [ptr len] string/slice (ffi/pointer-buffer ptr len len))

(defn get-message [sock]
  (def msgsize (pollnet-get-data-size gctx sock))
  (if (> msgsize 0) 
    (copy-cdata (pollnet-unsafe-get-data-ptr gctx sock) msgsize) 
    ""))

(defn rand-int [m] (math/floor (* (math/random) m)))

(def sock (pollnet-open-ws gctx "wss://irc-ws.chat.twitch.tv:443"))
(pollnet-send gctx sock (string "NICK justinfan" (rand-int 100000)))
(pollnet-send gctx sock (string "JOIN #" twitch-chan))

(var happy true)
(while happy 
  (def status (pollnet-update gctx sock))
  (set happy (>= status 3))
  (if (= status 5)
    (print-irc-msg (get-message sock))
  )
  (ev/sleep 0.1))
(print "closed:" (get-message sock))