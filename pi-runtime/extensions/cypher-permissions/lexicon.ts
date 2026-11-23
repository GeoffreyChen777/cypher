/**
 * The words the explicit-request check reads, in many languages, as one
 * table: what asks for each action (merge, push, release, delete, …),
 * what names each kind of thing (a pull request, a branch, a tag, …), what
 * says not to or not yet (negation, postponement, retraction), what is a
 * reminder rather than a "don't" ("don't forget to merge"), what only
 * carries on, and the words that mark a branch as where a pull request
 * goes (its base) or comes from (its head).
 *
 * The check never matches a phrase of one language in code: it asks this
 * table. A language is a row; adding one is adding data. Words in
 * scripts with spaces (Latin, Cyrillic) match at a word start, a trailing
 * `*` making them stems (`merg*`: merge, merged, mergen); words in CJK and
 * Hangul match anywhere. A message written mostly in a script no row
 * covers can't be read for a "don't", so the check asks (`uncoveredScript`).
 */

export type ActionKey =
  | "create" | "delete" | "merge" | "close" | "comment" | "review" | "edit" | "ready" | "reopen" | "push" | "publish" | "release"
  | "unpublish" | "install" | "stop" | "erase" | "apply" | "rename" | "write" | "archive" | "reach";

export type NounKey =
  | "pr" | "branch" | "tag" | "version" | "package" | "host" | "repo" | "remote" | "path" | "worktree" | "process" | "simulator" | "container" | "image" | "resource";

export interface Language {
  code: string;
  actions: Partial<Record<ActionKey, readonly string[]>>;
  nouns: Partial<Record<NounKey, readonly string[]>>;
  /** Says not to do what follows (or, in verb-final languages, precedes) it. */
  negate: readonly string[];
  /** Says to wait, stop, not yet, cancel, or do something else instead. */
  retract: readonly string[];
  /** A "don't" that asks for the thing ("don't forget"). */
  remind: readonly string[];
  /** Only carries on ("ok", "continue"). */
  carryOn: readonly string[];
  /** Small words a bare "wait" may come with and still be bare. */
  filler: readonly string[];
  /** Prefers something else or defers it ("rather", "instead", "later", "for now", "tomorrow"). Counted with `retract`. */
  defer?: readonly string[];
  /** Mark the branch a pull request goes into: before it (`into main`) or after it (`mainに`). */
  baseBefore?: readonly string[];
  baseAfter?: readonly string[];
  /** Mark the branch a pull request comes from. */
  headBefore?: readonly string[];
  headAfter?: readonly string[];
}

export const LANGUAGES: readonly Language[] = [
  {
    code: "en",
    actions: {
      create: ["open*", "create*", "make*", "file", "raise*", "submit*", "send*", "new", "set up", "add*", "init*"],
      delete: ["delet*", "remov*", "drop*", "prune*", "clean up", "cleanup", "tidy*", "rm"],
      merge: ["merg*"],
      close: ["clos*"],
      comment: ["comment*", "reply*"],
      review: ["review*", "approv*"],
      edit: ["edit*", "title", "body", "description", "chang*", "updat*"],
      ready: ["ready", "draft"],
      reopen: ["reopen*"],
      push: ["push*", "upload*"],
      publish: ["publish*", "releas*", "upload*"],
      release: ["releas*", "publish*", "ship*", "tag*", "cut a"],
      unpublish: ["unpublish*", "yank*", "withdraw*", "deprecat*"],
      install: ["install*"],
      stop: ["stop*", "kill*", "shut down", "shutdown", "halt*", "terminat*"],
      erase: ["eras*", "wip*", "reset*", "clear*"],
      apply: ["appl*", "deploy*", "scal*", "roll out", "rollout", "restart*", "upgrad*", "install*", "uninstall*", "roll back", "rollback"],
      rename: ["renam*"],
      write: ["writ*", "add*", "append*", "edit*", "chang*", "updat*", "set", "put", "sav*"],
      archive: ["archiv*"],
      reach: ["deploy*", "ssh", "connect*", "log in", "login", "run*"],
    },
    nouns: {
      pr: ["pr", "prs", "pull request*", "pull-request*", "pullrequest*", "mr", "mrs", "merge request*"],
      branch: ["branch*", "ref"],
      tag: ["tag*"],
      version: ["version*", "release*"],
      package: ["package*", "crate*"],
      host: ["server*", "host*", "machine*", "cluster*", "box"],
      repo: ["repo", "repos", "repositor*"],
      remote: ["remote*"],
      path: ["file*", "folder*", "dir", "dirs", "director*"],
      worktree: ["worktree*", "checkout*"],
      process: ["process*"],
      simulator: ["simulator*", "device*"],
      container: ["container*"],
      image: ["image*"],
      resource: ["deployment*", "service*", "pod", "pods", "chart*", "release*"],
    },
    negate: ["don't", "dont", "do not", "never", "not yet", "no need to", "without", "instead of", "hold off", "skip"],
    retract: ["wait*", "hold on", "hold off", "don't", "dont", "do not", "stop", "cancel*", "abort*", "never mind", "nevermind", "not yet", "not now", "no longer", "revert*", "undo", "instead", "scratch that", "forget it", "forget that", "postpone*", "later"],
    remind: ["don't forget", "do not forget", "never forget", "don't hesitate", "don't worry", "don't be afraid"],
    carryOn: ["ok", "okay", "yes", "yep", "yeah", "sure", "go ahead", "go on", "go", "continue", "proceed", "carry on", "keep going", "try again", "try it again", "retry", "again", "all set", "done", "fixed", "ready", "looks good", "lgtm", "sounds good", "fine", "good", "great", "perfect", "thanks", "thank you"],
    filler: ["it", "that", "this", "them", "those", "these", "yet", "now", "for now", "please", "pls", "just", "ok", "okay", "so", "and", "but", "oh", "hey", "actually", "no", "nope", "sorry", "oops", "well", "hmm", "a sec", "a second", "a moment", "a minute", "a bit", "do", "doing", "anything", "one", "more", "on", "up", "the", "all"],
    baseBefore: ["into", "onto", "to", "against", "targeting", "base", "on"],
    headBefore: ["from", "head"],
  },
  {
    code: "zh",
    actions: {
      create: ["开", "提", "建", "创建", "新建", "发", "做一个", "弄一个"],
      delete: ["删除", "删掉", "删了", "删", "移除", "清理", "清除"],
      merge: ["合并", "合入", "合进", "并入", "合到", "合了"],
      close: ["关闭", "关掉"],
      comment: ["评论", "留言", "回复"],
      review: ["审查", "审核", "批准"],
      edit: ["修改", "改"],
      ready: ["就绪"],
      reopen: ["重新打开", "重开"],
      push: ["推送", "推到", "推上", "推一下", "推", "上传"],
      publish: ["发布", "发版", "上传"],
      release: ["发布", "发版", "发新版", "上线", "打标签", "标签"],
      unpublish: ["撤回", "下架", "撤销", "弃用"],
      install: ["安装", "装上", "装一下", "装个"],
      stop: ["停止", "停掉", "关闭", "关掉", "杀"],
      erase: ["清空", "重置", "抹掉"],
      apply: ["部署", "上线", "扩容", "缩容", "重启", "升级", "回滚", "卸载"],
      rename: ["重命名", "改名"],
      write: ["写", "加", "添加", "追加", "修改", "改", "更新", "保存"],
      archive: ["归档"],
      reach: ["登录", "连接", "部署", "执行"],
    },
    nouns: {
      pr: ["合并请求", "拉取请求"],
      branch: ["分支"],
      tag: ["标签"],
      version: ["版本", "新版"],
      package: ["包"],
      host: ["服务器", "主机", "集群", "机器"],
      repo: ["仓库"],
      remote: ["远程", "远端"],
      path: ["文件", "目录"],
      worktree: ["工作树"],
      process: ["进程"],
      simulator: ["模拟器", "设备"],
      container: ["容器"],
      image: ["镜像"],
      resource: ["资源", "服务", "集群"],
    },
    negate: ["先别", "别", "不要", "不用", "先不", "不必", "无需", "不需要", "暂时不", "还不"],
    retract: ["先别", "别再", "别急", "别动", "别推", "别合", "别发", "别做", "别弄", "别改", "别管", "不要", "不用", "先停", "停下", "停一下", "等等", "等一下", "等下", "取消", "算了", "暂停", "先不", "撤销", "回滚", "改成", "改为", "换成", "还不", "暂时不", "以后再"],
    remind: ["别忘了", "别忘记", "别忘", "不要忘了", "不要忘记", "别担心", "不用担心", "别犹豫"],
    carryOn: ["继续", "接着", "好", "嗯", "对", "是", "可以", "行", "没问题", "再试", "重试", "试试", "再来", "搞定", "弄好", "修好", "完成", "谢谢"],
    filler: ["这个", "那个", "这些", "那些", "它", "一下", "吧", "了", "先", "请", "再", "啊", "哦", "呢", "做", "弄", "搞"],
    baseBefore: ["给", "到", "合到", "合入", "合进", "进", "往"],
    headBefore: ["从"],
  },
  {
    code: "ja",
    actions: {
      create: ["作成", "作って", "作る", "出して", "出す", "開いて", "立てて", "新規"],
      delete: ["削除", "消して", "消す", "除去", "片付け"],
      merge: ["マージ", "統合", "取り込"],
      close: ["クローズ", "閉じ"],
      comment: ["コメント", "返信"],
      review: ["レビュー", "承認"],
      edit: ["編集", "修正", "変更"],
      push: ["プッシュ", "push"],
      publish: ["公開", "パブリッシュ", "リリース"],
      release: ["リリース", "公開", "タグ"],
      install: ["インストール", "導入"],
      stop: ["停止", "止めて", "終了", "キル"],
      erase: ["消去", "リセット", "初期化"],
      apply: ["デプロイ", "適用", "再起動", "アップグレード", "ロールバック"],
      rename: ["名前変更", "リネーム"],
      write: ["書き込", "追加", "保存", "更新"],
      archive: ["アーカイブ"],
      reach: ["接続", "ログイン", "実行"],
    },
    nouns: {
      pr: ["プルリク", "プルリクエスト", "マージリクエスト"],
      branch: ["ブランチ"],
      tag: ["タグ"],
      version: ["バージョン", "新版"],
      package: ["パッケージ"],
      host: ["サーバー", "サーバ", "ホスト", "マシン", "クラスタ"],
      repo: ["リポジトリ", "レポジトリ"],
      remote: ["リモート"],
      path: ["ファイル", "フォルダ", "ディレクトリ"],
      process: ["プロセス"],
      simulator: ["シミュレータ", "デバイス"],
      container: ["コンテナ"],
      image: ["イメージ"],
    },
    negate: ["しないで", "ないで", "しないでください", "まだ", "やめて", "不要", "いらない", "せずに", "しなくて"],
    retract: ["待って", "待て", "ちょっと待", "まだ", "やめて", "やめ", "中止", "キャンセル", "取り消", "後で", "あとで", "代わりに", "しないで", "ないで", "ストップ"],
    remind: ["忘れずに", "忘れないで"],
    carryOn: ["続けて", "続行", "はい", "オーケー", "了解", "お願いします", "いいよ", "大丈夫", "ありがとう"],
    filler: ["ちょっと", "ください", "して", "それ", "これ", "今", "は", "を", "ね", "よ"],
    baseAfter: ["に", "へ", "向け"],
    headAfter: ["から"],
  },
  {
    code: "ko",
    actions: {
      create: ["만들어", "만들기", "생성", "올려", "열어", "작성"],
      delete: ["삭제", "지워", "제거", "정리"],
      merge: ["병합", "머지", "합쳐", "합치"],
      close: ["닫아", "종료"],
      comment: ["댓글", "코멘트", "답글"],
      review: ["리뷰", "승인", "검토"],
      edit: ["수정", "편집", "변경"],
      push: ["푸시", "푸쉬", "올려"],
      publish: ["배포", "퍼블리시", "공개", "릴리스", "릴리즈"],
      release: ["릴리스", "릴리즈", "배포", "태그"],
      install: ["설치"],
      stop: ["중지", "멈춰", "정지", "꺼", "종료"],
      erase: ["초기화", "지우기", "리셋"],
      apply: ["배포", "적용", "재시작", "업그레이드", "롤백"],
      rename: ["이름 변경", "이름 바꿔"],
      write: ["쓰기", "추가", "저장", "업데이트"],
      archive: ["보관"],
      reach: ["접속", "연결", "로그인", "실행"],
    },
    nouns: {
      pr: ["풀 리퀘스트", "풀리퀘스트", "풀리퀘", "병합 요청", "머지 리퀘스트"],
      branch: ["브랜치"],
      tag: ["태그"],
      version: ["버전", "새 버전"],
      package: ["패키지"],
      host: ["서버", "호스트", "머신", "클러스터"],
      repo: ["저장소", "리포지토리", "레포"],
      remote: ["리모트", "원격"],
      path: ["파일", "폴더", "디렉터리", "디렉토리"],
      process: ["프로세스"],
      simulator: ["시뮬레이터", "기기"],
      container: ["컨테이너"],
      image: ["이미지"],
    },
    negate: ["하지 마", "지 마", "하지마", "지마", "말고", "아직", "하지 말", "안 돼", "필요 없"],
    retract: ["하지 마", "지 마", "하지마", "지마", "말고", "아직", "기다려", "잠깐", "잠시만", "취소", "멈춰", "중지", "그만", "나중에", "대신", "보류"],
    remind: ["잊지 마", "잊지마", "잊지 말고", "걱정 마"],
    carryOn: ["계속", "진행", "네", "응", "좋아", "오케이", "알겠", "감사"],
    filler: ["좀", "그거", "이거", "지금", "요", "줘", "주세요"],
    baseAfter: ["에", "으로", "로"],
    headAfter: ["에서"],
  },
  {
    code: "de",
    actions: {
      create: ["erstell*", "öffne*", "anleg*", "leg an", "mach*", "eröffne*", "aufmach*"],
      delete: ["lösch*", "entfern*", "aufräum*", "räum auf", "weg"],
      merge: ["merg*", "zusammenführ*", "führ zusammen", "führe zusammen", "einpfleg*"],
      close: ["schließ*", "schliess*"],
      comment: ["kommentier*", "antwort*"],
      review: ["review*", "prüf*", "genehmig*", "freigeb*"],
      edit: ["bearbeit*", "änder*", "aktualisier*"],
      push: ["push*", "hochlad*", "lad hoch", "lade hoch"],
      publish: ["veröffentlich*", "publizier*", "releas*"],
      release: ["releas*", "veröffentlich*", "tagg*", "ausliefer*"],
      unpublish: ["zurückzieh*"],
      install: ["installier*"],
      stop: ["stopp*", "beend*", "anhalt*", "kill*"],
      erase: ["zurücksetz*", "leer*"],
      apply: ["deploy*", "ausroll*", "skalier*", "neu start*", "neustart*"],
      rename: ["umbenenn*"],
      write: ["schreib*", "hinzufüg*", "speicher*"],
      archive: ["archivier*"],
      reach: ["verbind*", "einlogg*", "anmeld*", "ausführ*"],
    },
    nouns: {
      pr: ["pull-request*", "merge-request*", "zusammenführungsanfrage*"],
      branch: ["branch*", "zweig*"],
      tag: ["tag*"],
      version: ["version*"],
      package: ["paket*"],
      host: ["server*", "host*", "rechner*", "maschine*", "cluster*"],
      repo: ["repo*", "repositor*"],
      path: ["datei*", "ordner*", "verzeichnis*"],
      process: ["prozess*"],
      container: ["container*"],
    },
    negate: ["nicht", "nie", "kein*", "noch nicht", "ohne", "statt", "anstatt", "lieber nicht"],
    retract: ["wart*", "halt", "stopp*", "nicht", "noch nicht", "abbrech*", "brich ab", "lass das", "lass es", "lieber nicht", "stattdessen", "später", "rückgängig", "vergiss es", "moment"],
    remind: ["vergiss nicht", "nicht vergessen", "keine sorge"],
    carryOn: ["ok", "okay", "ja", "weiter", "mach weiter", "fortfahren", "los", "gut", "passt", "danke", "perfekt"],
    filler: ["mal", "kurz", "bitte", "doch", "das", "es", "jetzt", "erst", "eben", "noch"],
    baseBefore: ["in", "nach", "auf", "gegen", "ziel"],
    headBefore: ["von", "aus"],
  },
  {
    code: "fr",
    actions: {
      create: ["crée*", "créer", "créez", "ouvr*", "fais une", "faire une", "soumet*", "lance*"],
      delete: ["supprim*", "efface*", "enlève*", "retire*", "nettoie*"],
      merge: ["fusionn*", "merg*", "intègr*"],
      close: ["ferm*", "clôtur*"],
      comment: ["comment*", "répond*"],
      review: ["relis*", "relire", "approuv*", "valide*"],
      edit: ["modifi*", "édite*", "change*", "mets à jour"],
      push: ["pousse*", "push*", "envoie*", "téléverse*"],
      publish: ["publi*", "sors", "sortir", "livre*"],
      release: ["publi*", "sors", "sortir", "livre*", "tague*", "release*"],
      unpublish: ["dépublie*", "retire*"],
      install: ["install*"],
      stop: ["arrête*", "stoppe*", "tue*", "termine*"],
      erase: ["efface*", "réinitialis*", "vide*"],
      apply: ["déploie*", "déployer", "applique*", "redémarre*", "mets à l'échelle"],
      rename: ["renomm*"],
      write: ["écri*", "ajoute*", "enregistre*"],
      archive: ["archive*"],
      reach: ["connecte*", "exécute*"],
    },
    nouns: {
      pr: ["demande de fusion", "demande de tirage", "demande d'extraction", "pull request*"],
      branch: ["branche*"],
      tag: ["tag*", "étiquette*"],
      version: ["version*"],
      package: ["paquet*", "package*"],
      host: ["serveur*", "hôte*", "machine*", "cluster*"],
      repo: ["dépôt*", "repo*"],
      path: ["fichier*", "dossier*", "répertoire*"],
      process: ["processus"],
      container: ["conteneur*"],
    },
    negate: ["ne", "n'", "pas", "jamais", "pas encore", "sans", "au lieu de", "plutôt que"],
    retract: ["attend*", "pas encore", "ne", "n'", "pas", "arrête*", "stop", "annul*", "laisse tomber", "plutôt", "au lieu de", "plus tard", "oublie ça", "minute"],
    remind: ["n'oublie pas", "n'oubliez pas", "ne t'inquiète pas", "ne vous inquiétez pas", "n'hésite pas"],
    carryOn: ["ok", "oui", "d'accord", "vas-y", "allez", "continue*", "poursui*", "parfait", "merci", "bien"],
    filler: ["un peu", "une seconde", "s'il te plaît", "stp", "svp", "ça", "ce", "le", "la", "les", "maintenant", "encore", "fais", "faire"],
    baseBefore: ["dans", "vers", "sur", "contre"],
    headBefore: ["depuis", "de"],
  },
  {
    code: "es",
    actions: {
      create: ["crea*", "abre", "abrir", "abra", "haz un", "haz una", "envía*", "levanta*"],
      delete: ["borra*", "elimina*", "quita*", "limpia*"],
      merge: ["fusiona*", "merge*", "haz merge", "integra*", "mezcla*"],
      close: ["cierra*", "cerrar"],
      comment: ["comenta*", "responde*"],
      review: ["revisa*", "aprueba*"],
      edit: ["edita*", "modifica*", "cambia*", "actualiza*"],
      push: ["empuja*", "sube", "subir", "push*", "haz push"],
      publish: ["publica*", "lanza*", "saca*"],
      release: ["publica*", "lanza*", "saca*", "etiqueta*", "release*"],
      unpublish: ["despublica*", "retira*"],
      install: ["instala*"],
      stop: ["detén", "detener", "para el", "mata*", "termina*"],
      erase: ["borra*", "reinicia*", "vacía*"],
      apply: ["despliega*", "desplegar", "aplica*", "escala*", "reinicia*"],
      rename: ["renombra*"],
      write: ["escribe*", "añade*", "agrega*", "guarda*"],
      archive: ["archiva*"],
      reach: ["conecta*", "ejecuta*"],
    },
    nouns: {
      pr: ["solicitud de extracción", "solicitud de fusión", "solicitud de cambios", "pull request*"],
      branch: ["rama*"],
      tag: ["etiqueta*", "tag*"],
      version: ["versión", "versiones", "version*"],
      package: ["paquete*"],
      host: ["servidor*", "máquina*", "clúster*", "cluster*"],
      repo: ["repositorio*", "repo*"],
      path: ["archivo*", "carpeta*", "directorio*"],
      process: ["proceso*"],
      container: ["contenedor*"],
    },
    negate: ["no", "nunca", "todavía no", "aún no", "sin", "en vez de", "en lugar de"],
    retract: ["espera*", "no", "todavía no", "aún no", "detente", "alto", "cancela*", "olvídalo", "mejor no", "en vez de", "en lugar de", "más tarde", "luego"],
    remind: ["no olvides", "no te olvides", "no te preocupes", "no dudes"],
    carryOn: ["ok", "sí", "si", "vale", "dale", "sigue", "continúa", "continua", "adelante", "perfecto", "gracias", "bien"],
    filler: ["un momento", "un segundo", "por favor", "eso", "esto", "lo", "la", "ahora", "todavía", "aún", "hazlo"],
    baseBefore: ["a", "hacia", "en", "contra"],
    headBefore: ["desde", "de"],
  },
  {
    code: "pt",
    actions: {
      create: ["cria*", "crie", "criar", "abra", "abre", "abrir", "faça um", "faz um", "envie"],
      delete: ["apag*", "exclu*", "remov*", "delet*", "limp*"],
      merge: ["mescl*", "funde", "fundir", "fundi", "faz merge", "faça merge", "merge*", "integr*"],
      close: ["fech*"],
      comment: ["coment*", "respond*"],
      review: ["revis*", "aprov*"],
      edit: ["edit*", "modifiq*", "modific*", "mude", "mudar", "atualiz*"],
      push: ["empurr*", "suba", "sobe", "subir", "push*", "envi*"],
      publish: ["publiq*", "public*", "lanc*"],
      release: ["publiq*", "public*", "lanc*", "release*", "etiquet*"],
      install: ["instal*"],
      stop: ["pare", "parar", "interromp*", "mate", "encerr*"],
      erase: ["apag*", "reinici*", "limp*"],
      apply: ["implant*", "aplic*", "reinici*", "escal*"],
      rename: ["renome*"],
      write: ["escrev*", "adicion*", "salv*"],
      archive: ["arquiv*"],
      reach: ["conect*", "execut*"],
    },
    nouns: {
      pr: ["pull request*", "solicitação de pull", "solicitação de merge", "pedido de merge"],
      branch: ["branch*", "ramo*"],
      tag: ["tag*", "etiqueta*"],
      version: ["versão", "versões"],
      package: ["pacote*"],
      host: ["servidor*", "máquina*", "cluster*"],
      repo: ["repositório*", "repo*"],
      path: ["arquivo*", "ficheiro*", "pasta*", "diretório*"],
      container: ["contêiner*", "container*"],
    },
    negate: ["não", "nunca", "ainda não", "sem", "em vez de", "ao invés de"],
    retract: ["espere", "espera", "aguard*", "não", "ainda não", "pare", "cancel*", "esquece", "deixa pra lá", "em vez de", "ao invés de", "depois", "mais tarde"],
    remind: ["não esqueça", "não se esqueça", "não esquece", "não se preocupe"],
    carryOn: ["ok", "sim", "pode", "continua", "continue", "segue", "vai", "perfeito", "obrigado", "obrigada", "beleza"],
    filler: ["um momento", "um segundo", "por favor", "isso", "isto", "agora", "ainda", "faz", "faça"],
    baseBefore: ["para", "pra", "em", "no", "na"],
    headBefore: ["de", "do", "da", "desde"],
  },
  {
    code: "it",
    actions: {
      create: ["crea*", "apri*", "fai una", "fai un", "invia*"],
      delete: ["cancell*", "elimin*", "rimuov*", "togli*", "pulisc*"],
      merge: ["unisci", "unire", "fondi", "fondere", "merg*", "integr*"],
      close: ["chiud*"],
      comment: ["comment*", "rispond*"],
      review: ["revision*", "approv*"],
      edit: ["modific*", "aggiorn*", "cambi*"],
      push: ["pusha*", "invia*", "carica*", "push*"],
      publish: ["pubblic*", "rilascia*"],
      release: ["pubblic*", "rilascia*", "tagga*", "release*"],
      install: ["install*"],
      stop: ["ferma*", "arresta*", "termina*", "uccidi"],
      erase: ["cancella*", "azzera*", "resetta*"],
      apply: ["deploya*", "distribuisci", "applica*", "riavvia*", "scala*"],
      rename: ["rinomin*"],
      write: ["scriv*", "aggiung*", "salva*"],
      archive: ["archivia*"],
      reach: ["connett*", "esegui*"],
    },
    nouns: {
      pr: ["pull request*", "richiesta di pull", "richiesta di merge"],
      branch: ["branch*", "ramo", "rami"],
      tag: ["tag*", "etichett*"],
      version: ["version*"],
      package: ["pacchett*"],
      host: ["server*", "macchina*", "cluster*"],
      repo: ["repository", "repo*"],
      path: ["file", "cartell*", "directory"],
      container: ["container*", "contenitor*"],
    },
    negate: ["non", "mai", "non ancora", "senza", "invece di"],
    retract: ["aspett*", "fermati", "ferma", "non", "non ancora", "annulla*", "lascia perdere", "invece", "più tardi", "dopo"],
    remind: ["non dimenticare", "non dimenticarti", "non preoccuparti"],
    carryOn: ["ok", "sì", "si", "va bene", "vai", "continua", "procedi", "perfetto", "grazie", "bene"],
    filler: ["un attimo", "un momento", "per favore", "questo", "quello", "ora", "adesso", "ancora", "fallo"],
    baseBefore: ["in", "verso", "su", "contro"],
    headBefore: ["da", "dal", "dalla"],
  },
  {
    code: "ru",
    actions: {
      create: ["созда*", "открой", "откр*", "сделай", "заведи", "подними"],
      delete: ["удали*", "удалить", "снеси", "убери", "почисти"],
      merge: ["смерж*", "мерж*", "слей", "слить", "влей", "влить", "объедини*"],
      close: ["закрой", "закрыть"],
      comment: ["прокомментир*", "ответь"],
      review: ["проверь", "одобри*", "ревью"],
      edit: ["измени*", "отредактир*", "обнови*", "поправь"],
      push: ["запуш*", "пушни*", "пуш*", "отправ*", "залей", "залить"],
      publish: ["опубликуй", "опубликовать", "публику*", "выпусти*", "выкати*"],
      release: ["релиз*", "зарелиз*", "выпусти*", "выкати*", "тег*", "опубликуй"],
      install: ["установи*", "постав*"],
      stop: ["останови*", "убей", "заверши*", "выключи*"],
      erase: ["сотри", "сбрось", "очисти"],
      apply: ["задеплой*", "деплой*", "разверни*", "примени*", "перезапусти*", "масштабир*"],
      rename: ["переименуй*"],
      write: ["запиши", "добавь", "сохрани"],
      archive: ["архивир*"],
      reach: ["подключи*", "зайди", "выполни"],
    },
    nouns: {
      pr: ["пулл-реквест*", "пул-реквест*", "пулреквест*", "пул реквест*", "пулл реквест*", "запрос на слияние", "мерж-реквест*", "пр"],
      branch: ["ветк*", "бранч*"],
      tag: ["тег*"],
      version: ["верси*"],
      package: ["пакет*"],
      host: ["сервер*", "хост*", "машин*", "кластер*"],
      repo: ["репозитор*", "репо"],
      path: ["файл*", "папк*", "каталог*", "директор*"],
      process: ["процесс*"],
      container: ["контейнер*"],
    },
    negate: ["не", "нет", "никогда", "пока не", "ещё не", "еще не", "без", "вместо"],
    retract: ["подожди*", "погоди*", "постой", "стоп", "не", "пока не", "ещё не", "еще не", "отмени*", "отмена", "не надо", "вместо", "позже", "потом", "забудь"],
    remind: ["не забудь", "не забудьте", "не волнуйся", "не стесняйся"],
    carryOn: ["ок", "ok", "да", "давай", "продолжай", "продолжи", "дальше", "отлично", "спасибо", "хорошо"],
    filler: ["пожалуйста", "это", "его", "сейчас", "пока", "секунду", "минуту", "чуть"],
    baseBefore: ["в", "во", "на"],
    headBefore: ["из", "от"],
  },
  {
    code: "nl",
    actions: {
      create: ["maak*", "open*", "aanmak*"],
      delete: ["verwijder*", "wis*", "ruim op"],
      merge: ["merg*", "samenvoeg*", "voeg samen"],
      push: ["push*"],
      publish: ["publiceer*", "releas*"],
      release: ["releas*", "publiceer*", "tag*"],
      install: ["installeer*"],
      stop: ["stop*", "beëindig*"],
      apply: ["deploy*", "herstart*"],
    },
    nouns: { pr: ["pull request*"], branch: ["branch*", "tak"], tag: ["tag*"], version: ["versie*"], repo: ["repo*", "repository"], host: ["server*"] },
    negate: ["niet", "nog niet", "nooit", "zonder", "in plaats van"],
    retract: ["wacht*", "niet", "nog niet", "stop", "annuleer*", "laat maar", "in plaats van", "later"],
    remind: ["vergeet niet", "niet vergeten"],
    carryOn: ["ok", "ja", "ga door", "doorgaan", "prima", "bedankt"],
    filler: ["even", "alsjeblieft", "dat", "het", "nu"],
    baseBefore: ["naar", "in", "op"],
    headBefore: ["van", "uit"],
  },
  {
    code: "pl",
    actions: {
      create: ["utwórz", "stwórz", "otwórz", "załóż", "zrób"],
      delete: ["usuń", "skasuj", "wyczyść"],
      merge: ["zmerguj", "scal*", "połącz", "merg*"],
      push: ["wypchnij", "pushnij", "push*", "wyślij"],
      publish: ["opublikuj", "wydaj"],
      release: ["wydaj", "opublikuj", "otaguj"],
      install: ["zainstaluj"],
      stop: ["zatrzymaj", "zabij"],
      apply: ["wdróż", "zrestartuj"],
    },
    nouns: { pr: ["pull request*"], branch: ["gałąź", "gałęzi*", "branch*"], tag: ["tag*"], version: ["wersj*"], repo: ["repozytori*", "repo"], host: ["serwer*"] },
    negate: ["nie", "jeszcze nie", "nigdy", "bez", "zamiast"],
    retract: ["czekaj", "poczekaj", "zaczekaj", "nie", "jeszcze nie", "stop", "anuluj", "zamiast", "później", "potem"],
    remind: ["nie zapomnij"],
    carryOn: ["ok", "tak", "dalej", "kontynuuj", "dzięki", "super"],
    filler: ["proszę", "to", "teraz", "chwilę"],
    baseBefore: ["do", "na"],
    headBefore: ["z", "ze"],
  },
  {
    code: "tr",
    actions: {
      create: ["oluştur*", "aç", "açın"],
      delete: ["sil*", "kaldır*", "temizle*"],
      merge: ["birleştir*", "merge*"],
      push: ["push*", "gönder*"],
      publish: ["yayınla*"],
      release: ["yayınla*", "sürüm çıkar", "etiketle*"],
      install: ["kur", "yükle*"],
      stop: ["durdur*", "öldür*"],
      apply: ["dağıt*", "yeniden başlat"],
    },
    nouns: { pr: ["pull request*", "çekme isteği"], branch: ["dal*", "branch*"], tag: ["etiket*"], version: ["sürüm*"], repo: ["depo*", "repo*"], host: ["sunucu*"] },
    negate: ["yapma", "etme", "henüz", "olmadan", "yerine"],
    retract: ["bekle*", "yapma", "etme", "henüz", "dur", "iptal", "vazgeç*", "yerine", "sonra"],
    remind: ["unutma"],
    carryOn: ["tamam", "evet", "devam", "teşekkürler"],
    filler: ["lütfen", "şimdi", "bir saniye"],
    baseBefore: [],
    headBefore: [],
  },
];

/** Preference and deferral, per language (merged into the rows below). */
const DEFER: Readonly<Record<string, readonly string[]>> = {
  en: ["rather", "instead", "later", "for now", "for the moment", "tomorrow", "next week", "some other time", "another time", "leave it", "leave that", "let it be", "postpon*", "defer*", "delay*", "prefer*", "better to", "better not", "not today", "eventually", "on hold", "hold off"],
  zh: ["还是", "宁可", "不如", "改天", "明天", "以后", "晚点", "稍后", "先放着", "先留着", "留着", "放着", "推迟", "延后", "回头再", "暂时", "先不急", "过几天", "下周"],
  ja: ["むしろ", "代わりに", "後で", "あとで", "明日", "今は", "とりあえず", "一旦", "保留", "延期", "後回し", "そのままに", "置いておい", "来週", "今度"],
  ko: ["차라리", "대신", "나중에", "내일", "일단", "보류", "연기", "미뤄", "미루", "그대로 둬", "놔둬", "두자", "다음 주", "다음에"],
  de: ["lieber", "eher", "statt", "stattdessen", "vorerst", "vorläufig", "später", "morgen", "erstmal", "erst mal", "nächste woche", "lass", "lassen", "belass*", "verschieb*", "aufschieb*", "bevorzug*", "besser", "ein andermal", "irgendwann"],
  fr: ["plutôt", "au lieu", "plus tard", "demain", "pour l'instant", "pour le moment", "laisse", "laissez", "garde", "gardez", "reporte*", "repousse*", "préfère*", "mieux vaut", "la semaine prochaine", "une autre fois"],
  es: ["mejor", "en vez", "en lugar", "más tarde", "luego", "mañana", "por ahora", "por el momento", "deja", "dejar", "déjalo", "pospon*", "aplaza*", "prefiero", "prefiere*", "la próxima semana", "otro día"],
  pt: ["melhor", "em vez", "ao invés", "mais tarde", "depois", "amanhã", "por enquanto", "por agora", "deixa", "deixe", "adia*", "prefiro", "prefere", "semana que vem", "outro dia"],
  it: ["meglio", "piuttosto", "invece", "più tardi", "dopo", "domani", "per ora", "per adesso", "lascia", "lasciate", "rimanda*", "rinvia*", "preferisco", "la prossima settimana", "un'altra volta"],
  ru: ["лучше", "вместо", "позже", "потом", "завтра", "пока что", "оставь", "оставим", "отложи*", "отложим", "перенеси*", "перенесём", "предпочита*", "на следующей неделе", "в другой раз"],
  nl: ["liever", "in plaats", "later", "morgen", "voorlopig", "laat", "uitstel*", "volgende week"],
  pl: ["raczej", "lepiej", "zamiast", "później", "jutro", "na razie", "zostaw", "odłóż", "przełóż", "wolę"],
  tr: ["yerine", "sonra", "yarın", "şimdilik", "bırak", "ertele*", "tercih"],
};
for (const language of LANGUAGES as Language[]) {
  (language as { defer?: readonly string[] }).defer = DEFER[language.code] ?? [];
}

// --- Compiled --------------------------------------------------------------------

/** Letters of a script spaces separate: such words match at a word start. */
const SPACED = /[\p{Script=Latin}\p{Script=Cyrillic}\p{Script=Greek}]/u;
/** The same letters (and combining marks), as a character-class body. */
const SPACED_LETTER = "\\p{Script=Latin}\\p{Script=Cyrillic}\\p{Script=Greek}\\p{M}";

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** One word of the table as a regex source. */
function wordSource(word: string): string {
  const stem = word.endsWith("*");
  const body = (stem ? word.slice(0, -1) : word).normalize("NFKC").toLowerCase();
  const text = escape(body).replace(/\s+/g, "\\s+").replace(/'/g, "['’]?");
  if (!SPACED.test(body)) return text;
  // A word of a spaced script ends where letters of a spaced script end: next to a CJK character
  // ("然后pr", "就merge") it stands as a word of its own, as owners mix scripts without spaces.
  return `(?<![${SPACED_LETTER}\\p{N}_])${text}${stem ? "" : `(?![${SPACED_LETTER}\\p{N}_])`}`;
}

function compile(words: Iterable<string>, flags = "iu"): RegExp {
  const unique = [...new Set(words)].filter((word) => word.replace("*", "").trim());
  // Longest first, so a phrase wins over a word inside it.
  unique.sort((a, b) => b.length - a.length);
  return unique.length ? new RegExp(unique.map(wordSource).join("|"), flags) : /(?!)/u;
}

const all = <K extends string>(pick: (language: Language) => Partial<Record<K, readonly string[]>>, key: K) => LANGUAGES.flatMap((language) => pick(language)[key] ?? []);

const cache = new Map<string, RegExp>();
function cached(key: string, make: () => RegExp): RegExp {
  let regex = cache.get(key);
  if (!regex) cache.set(key, (regex = make()));
  return regex;
}

/** Words, in any language of the table, that ask for an action. */
export function actionWords(action: string, distinct = false): RegExp {
  // `distinct`: only words that can't be part of another word: a single CJK character ("建" in "建议") is left out.
  const keep = (word: string) => !distinct || SPACED.test(word) || [...word.replace(/\*$/, "")].length >= 2;
  return cached(`action:${action}:${distinct}`, () => compile(all((language) => language.actions, action as ActionKey).filter(keep)));
}

/** Words, in any language, that name a kind of thing. */
export function nounWords(noun: string): RegExp {
  return cached(`noun:${noun}`, () => compile(all((language) => language.nouns, noun as NounKey)));
}

type ListKey = "negate" | "retract" | "defer" | "remind" | "carryOn" | "filler" | "baseBefore" | "baseAfter" | "headBefore" | "headAfter";

/** One list of every language, as one regex (`g` for scanning). */
export function listWords(key: ListKey, global = false): RegExp {
  // A preference or deferral takes a request back as a "wait" does.
  const of = (language: Language) => (key === "retract" ? [...language.retract, ...(language.defer ?? [])] : (language[key] ?? []));
  return cached(`${key}:${global}`, () => compile(LANGUAGES.flatMap(of), global ? "giu" : "iu"));
}

/** Every word of the table, any class, any language. */
export function allWords(): RegExp {
  return cached("all", () =>
    compile(LANGUAGES.flatMap((language) => [...Object.values(language.actions), ...Object.values(language.nouns), language.negate, language.retract, language.remind, language.carryOn, language.filler, language.baseBefore ?? [], language.headBefore ?? [], language.baseAfter ?? [], language.headAfter ?? []].flat() as string[])),
  );
}

/** A message that is only a carry-on: one of the table's, at its start. */
export function carriesOnOnly(text: string): boolean {
  const words = LANGUAGES.flatMap((language) => language.carryOn);
  const regex = cached("carryOn:start", () => new RegExp(`^(?:${[...new Set(words)].sort((a, b) => b.length - a.length).map(wordSource).join("|")})`, "iu"));
  return regex.test(text.normalize("NFKC").trim());
}

/** Scripts the table reads, by Unicode script name. */
const COVERED = ["Latin", "Cyrillic", "Han", "Hiragana", "Katakana", "Hangul", "Common", "Inherited"];

/**
 * The script a message is mostly written in when no row covers it (say,
 * "Arabic"), else undefined: its "don't" can't be read, so the check asks.
 */
export function uncoveredScript(text: string): string | undefined {
  const counts = new Map<string, number>();
  for (const char of text) {
    if (!/\p{L}/u.test(char)) continue;
    const script = SCRIPTS.find(([, regex]) => regex.test(char))?.[0] ?? "an unknown script";
    if (COVERED.includes(script)) continue;
    counts.set(script, (counts.get(script) ?? 0) + 1);
  }
  const [worst] = [...counts].sort((a, b) => b[1] - a[1]);
  return worst && worst[1] >= 2 ? worst[0] : undefined;
}

const SCRIPTS: readonly [string, RegExp][] = [
  "Latin", "Cyrillic", "Han", "Hiragana", "Katakana", "Hangul", "Greek", "Arabic", "Hebrew", "Thai", "Devanagari", "Bengali", "Tamil", "Telugu",
  "Armenian", "Georgian", "Ethiopic", "Khmer", "Lao", "Myanmar", "Sinhala", "Gujarati", "Gurmukhi", "Kannada", "Malayalam", "Tibetan", "Mongolian",
  "Common", "Inherited",
].map((script) => [script, new RegExp(`\\p{Script_Extensions=${script}}`, "u")]);
