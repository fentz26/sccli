//! Interface languages. Columns everywhere: en, fr, zh (simplified), ja, ko, es.

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
pub enum Lang {
    En,
    Fr,
    Zh,
    Ja,
    Ko,
    Es,
}

/// Language codes offered in settings, in table order.
pub const CODES: [&str; 6] = ["en", "fr", "zh", "ja", "ko", "es"];
/// Each language's own name.
pub const NAMES: [&str; 6] = ["English", "Français", "中文", "日本語", "한국어", "Español"];

static LANG: AtomicUsize = AtomicUsize::new(0);

pub fn set_lang(l: Lang) {
    LANG.store(l as usize, Ordering::Relaxed);
}

pub fn parse_lang(s: &str) -> Option<Lang> {
    let code = s.to_lowercase();
    let code = code.split(['_', '-', '.', '@']).next()?;
    Some(match code {
        "en" => Lang::En,
        "fr" => Lang::Fr,
        "zh" => Lang::Zh,
        "ja" | "jp" => Lang::Ja,
        "ko" | "kr" => Lang::Ko,
        "es" => Lang::Es,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
pub enum K {
    Help,
    Loading,
    Vol,
    Queue,
    More,
    Resuming,
    Nothing,
    SignedIn,
    SignedOut,
    NotSignedIn,
    NoTracks,
    Skipped,
    StreamFailed,
    Shuffle,
    LoginPrompt,
    Playing,
    Paused,
    Stopped,
    TabQueue,
    TabSettings,
    SettingsHint,
    SetLanguage,
    SetQuality,
    SetRepeat,
    SetShuffle,
    SetVolume,
    SetPreload,
    SetSeek,
    SetResume,
    SetMemory,
    On,
    Off,
    Auto,
    All,
    One,
    Original,
    Best,
    Saver,
    Low,
    Normal,
    High,
    Protected,
    About,
    CheckUpdates,
    Checking,
    UpToDate,
    UpdateAvailable,
    UpdateFailed,
    Buffering,
    Stalled,
    Recovering,
    ConnectionLost,
    Reconnecting,
    ListIncomplete,
    TabSearch,
    TabPlaylists,
    FilterTyping,
    FilterActive,
    SearchPrompt,
    SearchHint,
    PlaylistHint,
    NoMatches,
    NoPlaylists,
    SavedShortcut,
}

const TABLE: [[&str; 6]; 64] = [
    // Help
    [
        "space pause/resume · n next · b prev · s stop · ←/→ seek · ↑/↓ volume · l list · , settings · q quit",
        "espace pause/lecture · n suivant · b précédent · s arrêt · ←/→ avancer/reculer · ↑/↓ volume · l liste · , réglages · q quitter",
        "空格 暂停/继续 · n 下一首 · b 上一首 · s 停止 · ←/→ 快退/快进 · ↑/↓ 音量 · l 列表 · , 设置 · q 退出",
        "スペース 一時停止/再開 · n 次へ · b 前へ · s 停止 · ←/→ シーク · ↑/↓ 音量 · l 一覧 · , 設定 · q 終了",
        "스페이스 일시정지/재생 · n 다음 · b 이전 · s 정지 · ←/→ 탐색 · ↑/↓ 볼륨 · l 목록 · , 설정 · q 종료",
        "espacio pausa/reanudar · n siguiente · b anterior · s detener · ←/→ buscar · ↑/↓ volumen · l lista · , ajustes · q salir",
    ],
    // Loading
    [
        "loading…",
        "chargement…",
        "加载中…",
        "読み込み中…",
        "불러오는 중…",
        "cargando…",
    ],
    // Vol
    ["vol", "vol", "音量", "音量", "볼륨", "vol"],
    // Queue ({n}, {more})
    [
        "{n} tracks{more}  ↑/↓ move · enter play · space pause · n/b skip · / filter · ctrl+f search · tab next · l/esc back",
        "{n} titres{more}  ↑/↓ déplacer · entrée lire · espace pause · n/b passer · / filtrer · ctrl+f chercher · tab suivant · l/échap retour",
        "{n} 首{more}  ↑/↓ 移动 · 回车 播放 · 空格 暂停 · n/b 切歌 · / 筛选 · ctrl+f 搜索 · tab 下一页 · l/esc 返回",
        "{n} 曲{more}  ↑/↓ 移動 · Enter 再生 · スペース 一時停止 · n/b スキップ · / 絞り込み · Ctrl+F 検索 · Tab 次へ · l/Esc 戻る",
        "{n}곡{more}  ↑/↓ 이동 · Enter 재생 · 스페이스 일시정지 · n/b 건너뛰기 · / 필터 · Ctrl+F 검색 · Tab 다음 · l/Esc 뒤로",
        "{n} pistas{more}  ↑/↓ mover · intro reproducir · espacio pausa · n/b saltar · / filtrar · ctrl+f buscar · tab siguiente · l/esc volver",
    ],
    // More
    [
        "+, still loading",
        "+, chargement en cours",
        "+，仍在加载",
        "+、読み込み中",
        "+, 불러오는 중",
        "+, cargando",
    ],
    // Resuming
    [
        "Resuming where you left off …",
        "Reprise là où vous vous étiez arrêté …",
        "正在从上次停止的地方继续播放…",
        "前回の続きから再生します…",
        "마지막으로 재생한 곡부터 이어서 재생합니다…",
        "Reanudando donde lo dejaste …",
    ],
    // Nothing to resume
    [
        "nothing to resume yet — play something first:",
        "rien à reprendre pour l'instant — lancez d'abord quelque chose :",
        "还没有可继续播放的内容——请先播放一些内容：",
        "再開できる履歴がありません。まず何か再生してください：",
        "이어서 재생할 기록이 없습니다. 먼저 무언가를 재생하세요:",
        "todavía no hay nada que reanudar — reproduce algo primero:",
    ],
    // SignedIn ({user})
    [
        "Signed in as {user}. Try: scc -p likes",
        "Connecté en tant que {user}. Essayez : scc -p likes",
        "已登录为 {user}。试试：scc -p likes",
        "{user} としてサインインしました。試してみてください：scc -p likes",
        "{user}(으)로 로그인했습니다. 실행해 보세요: scc -p likes",
        "Sesión iniciada como {user}. Prueba: scc -p likes",
    ],
    // SignedOut
    [
        "Signed out.",
        "Déconnecté.",
        "已退出登录。",
        "サインアウトしました。",
        "로그아웃했습니다.",
        "Sesión cerrada.",
    ],
    // NotSignedIn
    [
        "not signed in — run `scc login` first",
        "non connecté — lancez d'abord `scc login`",
        "尚未登录——请先运行 `scc login`",
        "サインインしていません。先に `scc login` を実行してください",
        "로그인되어 있지 않습니다. 먼저 `scc login`을 실행하세요",
        "sesión no iniciada — ejecuta primero `scc login`",
    ],
    // NoTracks
    [
        "no playable tracks found",
        "aucun titre lisible trouvé",
        "没有找到可播放的曲目",
        "再生できる曲が見つかりません",
        "재생할 수 있는 곡을 찾을 수 없습니다",
        "no se encontraron pistas reproducibles",
    ],
    // Skipped ({e})
    [
        "skipped: {e}",
        "ignoré : {e}",
        "已跳过：{e}",
        "スキップ：{e}",
        "건너뜀: {e}",
        "omitida: {e}",
    ],
    // StreamFailed
    [
        "Stream keeps failing, press enter to retry or n to skip",
        "Le flux échoue sans cesse, appuyez sur entrée pour réessayer ou n pour passer",
        "播放持续失败，按回车重试或按 n 跳过",
        "再生に失敗し続けています。Enter で再試行、n でスキップ",
        "재생이 계속 실패합니다. Enter로 다시 시도하거나 n으로 건너뛰세요",
        "La reproducción sigue fallando, pulsa intro para reintentar o n para saltar",
    ],
    // Shuffle
    [
        "Loading all tracks to shuffle …",
        "Chargement de tous les titres pour le mode aléatoire …",
        "正在加载全部曲目以随机播放…",
        "シャッフルのため全曲を読み込み中…",
        "셔플을 위해 모든 곡을 불러오는 중…",
        "Cargando todas las pistas para mezclar …",
    ],
    // LoginPrompt
    [
        "Paste your SoundCloud oauth_token.\nFind it: log in at soundcloud.com → DevTools → Storage/Application → Cookies → oauth_token\n",
        "Collez votre oauth_token SoundCloud.\nOù le trouver : connectez-vous sur soundcloud.com → DevTools → Stockage/Application → Cookies → oauth_token\n",
        "请粘贴你的 SoundCloud oauth_token。\n获取方式：在 soundcloud.com 登录 → 开发者工具 → 存储/应用 → Cookie → oauth_token\n",
        "SoundCloud の oauth_token を貼り付けてください。\n取得方法：soundcloud.com にログイン → 開発者ツール → ストレージ/アプリケーション → Cookie → oauth_token\n",
        "SoundCloud oauth_token을 붙여넣으세요.\n찾는 방법: soundcloud.com 로그인 → 개발자 도구 → 저장소/애플리케이션 → 쿠키 → oauth_token\n",
        "Pega tu oauth_token de SoundCloud.\nDónde encontrarlo: inicia sesión en soundcloud.com → DevTools → Almacenamiento/Aplicación → Cookies → oauth_token\n",
    ],
    // Playing / Paused / Stopped (terminal title)
    [
        "Playing",
        "Lecture en cours",
        "正在播放",
        "再生中",
        "재생 중",
        "Reproduciendo",
    ],
    ["Paused", "En pause", "已暂停", "一時停止中", "일시정지됨", "En pausa"],
    ["Stopped", "Arrêté", "已停止", "停止中", "정지됨", "Detenido"],
    // Tabs and settings hint
    ["Queue", "File d'attente", "播放队列", "キュー", "재생목록", "Cola"],
    ["Settings", "Réglages", "设置", "設定", "설정", "Ajustes"],
    [
        "↑/↓ move · ←/→ change · space pause · tab next · l/esc back",
        "↑/↓ déplacer · ←/→ modifier · espace pause · tab suivant · l/échap retour",
        "↑/↓ 移动 · ←/→ 更改 · 空格 暂停 · tab 下一页 · l/esc 返回",
        "↑/↓ 移動 · ←/→ 変更 · スペース 一時停止 · Tab 次へ · l/Esc 戻る",
        "↑/↓ 이동 · ←/→ 변경 · 스페이스 일시정지 · Tab 재생목록 · l/Esc 뒤로",
        "↑/↓ mover · ←/→ cambiar · espacio pausa · tab siguiente · l/esc volver",
    ],
    // Setting names
    ["Language", "Langue", "语言", "言語", "언어", "Idioma"],
    [
        "Audio quality",
        "Qualité audio",
        "音质",
        "音質",
        "음질",
        "Calidad de audio",
    ],
    ["Repeat", "Répéter", "循环", "リピート", "반복", "Repetir"],
    [
        "Shuffle at start",
        "Aléatoire au démarrage",
        "启动时随机播放",
        "起動時にシャッフル",
        "시작 시 셔플",
        "Aleatorio al iniciar",
    ],
    [
        "Start volume",
        "Volume au démarrage",
        "启动音量",
        "起動時の音量",
        "시작 볼륨",
        "Volumen inicial",
    ],
    [
        "Songs to preload",
        "Titres préchargés",
        "预加载歌曲数",
        "先読みする曲数",
        "미리 불러올 곡 수",
        "Pistas precargadas",
    ],
    [
        "Seek step",
        "Pas de déplacement",
        "快进步长",
        "シーク幅",
        "탐색 간격",
        "Salto de búsqueda",
    ],
    [
        "Resume inside song",
        "Reprendre dans le titre",
        "从歌曲中断处继续",
        "曲の途中から再開",
        "곡 중간부터 이어듣기",
        "Reanudar dentro de la pista",
    ],
    [
        "Memory use",
        "Mémoire utilisée",
        "内存占用",
        "メモリ使用量",
        "메모리 사용량",
        "Uso de memoria",
    ],
    // Setting values
    ["On", "Activé", "开", "オン", "켜기", "Activado"],
    ["Off", "Désactivé", "关", "オフ", "끄기", "Desactivado"],
    ["Auto", "Auto", "自动", "自動", "자동", "Auto"],
    ["All", "Tout", "全部", "全曲", "전체", "Todo"],
    ["One song", "Un titre", "单曲", "1曲", "한 곡", "Una pista"],
    [
        "Original file when allowed",
        "Fichier original si autorisé",
        "允许时使用原始文件",
        "可能なら元ファイル",
        "가능하면 원본 파일",
        "Archivo original si se permite",
    ],
    [
        "Best stream",
        "Meilleur flux",
        "最佳音质流",
        "最高音質ストリーム",
        "최고 음질 스트림",
        "Mejor calidad",
    ],
    [
        "Data saver",
        "Économie de données",
        "省流量",
        "データ節約",
        "데이터 절약",
        "Ahorro de datos",
    ],
    ["Low", "Faible", "低", "低", "낮음", "Baja"],
    ["Normal", "Normale", "普通", "標準", "보통", "Normal"],
    ["High", "Élevée", "高", "高", "높음", "Alta"],
    // Protected
    [
        "protected stream, skipped",
        "flux protégé, ignoré",
        "受保护的音频，已跳过",
        "保護された音声のためスキップ",
        "보호된 스트림, 건너뜀",
        "transmisión protegida, omitida",
    ],
    // About section of Settings
    ["About", "À propos", "关于", "情報", "정보", "Acerca de"],
    [
        "Check for updates",
        "Rechercher des mises à jour",
        "检查更新",
        "アップデートを確認",
        "업데이트 확인",
        "Buscar actualizaciones",
    ],
    [
        "Checking…",
        "Vérification…",
        "正在检查…",
        "確認中…",
        "확인 중…",
        "Comprobando…",
    ],
    [
        "You have the latest version",
        "Vous avez la dernière version",
        "已是最新版本",
        "最新バージョンです",
        "최신 버전입니다",
        "Tienes la última versión",
    ],
    [
        "Version {v} is available",
        "La version {v} est disponible",
        "新版本 {v} 可用",
        "バージョン {v} が利用可能です",
        "버전 {v}을(를) 사용할 수 있습니다",
        "La versión {v} está disponible",
    ],
    [
        "Could not check for updates",
        "Impossible de vérifier les mises à jour",
        "无法检查更新",
        "アップデートを確認できませんでした",
        "업데이트를 확인할 수 없습니다",
        "No se pudo buscar actualizaciones",
    ],
    // Playback states
    [
        "Buffering",
        "Mise en mémoire tampon",
        "缓冲中",
        "バッファリング中",
        "버퍼링 중",
        "Almacenando en búfer",
    ],
    ["Stalled", "Bloqué", "已停滞", "停止中", "멈춤", "Detenido"],
    [
        "Reconnecting",
        "Reconnexion",
        "正在重新连接",
        "再接続中",
        "다시 연결 중",
        "Reconectando",
    ],
    [
        "Connection lost, press enter to retry",
        "Connexion perdue, appuyez sur entrée pour réessayer",
        "连接已断开，按回车重试",
        "接続が切れました。Enter で再試行",
        "연결이 끊겼습니다. Enter로 다시 시도",
        "Conexión perdida, pulsa intro para reintentar",
    ],
    [
        "reconnecting {n}/{max}",
        "reconnexion {n}/{max}",
        "正在重新连接 {n}/{max}",
        "再接続中 {n}/{max}",
        "다시 연결 중 {n}/{max}",
        "reconectando {n}/{max}",
    ],
    [
        "list incomplete: {e}",
        "liste incomplète : {e}",
        "列表不完整：{e}",
        "リストが不完全です: {e}",
        "목록이 불완전합니다: {e}",
        "lista incompleta: {e}",
    ],
    // TabSearch
    ["Search", "Recherche", "搜索", "検索", "검색", "Buscar"],
    // TabPlaylists
    [
        "Playlists",
        "Playlists",
        "歌单",
        "プレイリスト",
        "플레이리스트",
        "Listas",
    ],
    // FilterTyping
    [
        "Filter: {q} ({n}/{total}) · enter keep · esc clear",
        "Filtre : {q} ({n}/{total}) · entrée garder · échap effacer",
        "筛选：{q} ({n}/{total}) · 回车确认 · esc 清除",
        "絞り込み: {q} ({n}/{total}) · Enter 確定 · Esc 解除",
        "필터: {q} ({n}/{total}) · Enter 적용 · Esc 지우기",
        "Filtro: {q} ({n}/{total}) · intro guardar · esc borrar",
    ],
    // FilterActive
    [
        "Filter: {q} ({n}/{total}) · / edit · enter play · esc clear",
        "Filtre : {q} ({n}/{total}) · / modifier · entrée lire · échap effacer",
        "筛选：{q} ({n}/{total}) · / 编辑 · 回车播放 · esc 清除",
        "絞り込み: {q} ({n}/{total}) · / 編集 · Enter 再生 · Esc 解除",
        "필터: {q} ({n}/{total}) · / 편집 · Enter 재생 · Esc 지우기",
        "Filtro: {q} ({n}/{total}) · / editar · intro reproducir · esc borrar",
    ],
    // SearchPrompt
    [
        "SoundCloud: {q} · enter search · esc cancel",
        "SoundCloud : {q} · entrée chercher · échap annuler",
        "SoundCloud：{q} · 回车搜索 · esc 取消",
        "SoundCloud: {q} · Enter 検索 · Esc キャンセル",
        "SoundCloud: {q} · Enter 검색 · Esc 취소",
        "SoundCloud: {q} · intro buscar · esc cancelar",
    ],
    // SearchHint
    [
        "{q} · ↑/↓ select · enter play results · / edit search",
        "{q} · ↑/↓ choisir · entrée lire · / rechercher",
        "{q} · ↑/↓ 选择 · 回车播放结果 · / 新搜索",
        "{q} · ↑/↓ 選択 · Enter 結果を再生 · / 新規検索",
        "{q} · ↑/↓ 선택 · Enter 결과 재생 · / 새 검색",
        "{q} · ↑/↓ elegir · intro reproducir · / nueva búsqueda",
    ],
    // PlaylistHint
    [
        "↑/↓ select · enter open · r refresh · tab next",
        "↑/↓ choisir · entrée ouvrir · r actualiser · tab suivant",
        "↑/↓ 选择 · 回车打开 · r 刷新 · tab 下一页",
        "↑/↓ 選択 · Enter 開く · r 更新 · Tab 次へ",
        "↑/↓ 선택 · Enter 열기 · r 새로고침 · Tab 다음",
        "↑/↓ elegir · intro abrir · r actualizar · tab siguiente",
    ],
    // NoMatches
    [
        "No matches",
        "Aucun résultat",
        "无匹配结果",
        "一致する結果がありません",
        "일치하는 결과 없음",
        "Sin coincidencias",
    ],
    // NoPlaylists
    [
        "No playlists · save a shortcut with scc alias <name> <url>",
        "Aucune playlist · scc alias <nom> <url>",
        "暂无歌单 · scc alias <名称> <网址>",
        "プレイリストなし · scc alias <名前> <URL>",
        "플레이리스트 없음 · scc alias <이름> <URL>",
        "Sin listas · scc alias <nombre> <url>",
    ],
    // SavedShortcut
    ["saved", "enregistré", "已保存", "保存済み", "저장됨", "guardado"],
];

pub fn tr(k: K) -> &'static str {
    TABLE[k as usize][LANG.load(Ordering::Relaxed)]
}

/// Picks the language: `-l`, saved setting, then the system locale.
pub fn detect_lang(flag: Option<&str>, saved: Option<&str>) -> Lang {
    let sys = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty());
    [flag.map(str::to_string), saved.map(str::to_string), sys]
        .into_iter()
        .flatten()
        .find_map(|c| parse_lang(&c))
        .unwrap_or(Lang::En)
}

/// Truncates to a terminal column width (CJK characters take two columns).
pub fn clip(s: &str, cols: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let (mut out, mut w) = (String::new(), 0);
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > cols {
            break;
        }
        w += cw;
        out.push(c);
    }
    out
}

/// Display width in terminal columns.
pub fn width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_language_codes_and_locales() {
        assert!(matches!(parse_lang("ja"), Some(Lang::Ja)));
        assert!(matches!(parse_lang("zh_CN.UTF-8"), Some(Lang::Zh)));
        assert!(matches!(parse_lang("ko-KR"), Some(Lang::Ko)));
        assert!(parse_lang("xx").is_none());
    }

    #[test]
    fn clips_by_display_width() {
        assert_eq!(clip("日本語", 4), "日本");
        assert_eq!(clip("日本語", 5), "日本");
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn every_string_is_translated() {
        assert!(TABLE.iter().flatten().all(|s| !s.is_empty()));
    }

    #[test]
    fn placeholders_exist_in_every_language() {
        for (row, key) in [
            (K::Queue, "{n}"),
            (K::Queue, "{more}"),
            (K::SignedIn, "{user}"),
            (K::Skipped, "{e}"),
            (K::UpdateAvailable, "{v}"),
            (K::Reconnecting, "{n}"),
            (K::Reconnecting, "{max}"),
            (K::ListIncomplete, "{e}"),
            (K::FilterTyping, "{q}"),
            (K::FilterTyping, "{n}"),
            (K::FilterTyping, "{total}"),
            (K::FilterActive, "{q}"),
            (K::FilterActive, "{n}"),
            (K::FilterActive, "{total}"),
            (K::SearchPrompt, "{q}"),
            (K::SearchHint, "{q}"),
        ] {
            assert!(TABLE[row as usize].iter().all(|s| s.contains(key)), "{key}");
        }
    }
}
