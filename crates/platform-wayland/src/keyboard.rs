//! xkb 解码层：合成器送来的 keymap + modifiers → 可打印字符。
//!
//! 取代旧的手写 evdev 码表（只有 26 字母 + 10 数字 + 4 标点、没有 shift 层，
//! `? ! @ : "` 等永远进不了引擎）。布局正确性整体交给 libxkbcommon。
//! 本模块不碰 wayland 连接：tests/xkb_decode.rs 直接喂 keymap 字符串断言。

use xkbcommon::xkb;

/// wl_keyboard/zwp keymap format：xkb V1 文本。与 `xkb::KEYMAP_FORMAT_TEXT_V1`
/// 数值相同（wayland 定义就是抄 xkb 的）。
pub const KEYMAP_FORMAT_XKB_V1: u32 = 1;

pub struct Keyboard {
    context: xkb::Context,
    state: Option<xkb::State>,
    /// 键图里 Control 的 mod index（set_keymap 时缓存）；MOD_INVALID = 键图没这个修饰。
    ctrl_mod: xkb::ModIndex,
    /// 键图里 Alt 的 mod index（同上；Alt 常是虚拟 mod，index ≥ 8）。
    alt_mod: xkb::ModIndex,
    /// Ctrl/Alt 旗标：两个来源收敛到本结构体——`note_key` 记 evdev 键码
    /// （grab 重建丢 Key 事件前的兜底），`update_mods` 每次用合成器送来的
    /// xkb 掩码重新推导（权威路径：虚拟键盘 wtype 不产生 29/56 物理键，
    /// 旧实现只靠键码记账 → 旗标恒 false → ctrl+c 被当拼音吞掉）。
    ctrl: bool,
    alt: bool,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            state: None,
            ctrl_mod: xkb::MOD_INVALID,
            alt_mod: xkb::MOD_INVALID,
            ctrl: false,
            alt: false,
        }
    }

    /// 装入合成器 keymap 事件送来的 XKB V1 文本键图。失败返回 false 且沿用旧键图。
    pub fn set_keymap(&mut self, text: &str) -> bool {
        match xkb::Keymap::new_from_string(
            &self.context,
            text.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            Some(keymap) => {
                self.ctrl_mod = keymap.mod_get_index("Control");
                self.alt_mod = keymap.mod_get_index("Alt");
                // 换键图 = 旧 xkb 状态作废：旗标清零，等下一条 Modifiers 事件
                // 经 update_mods 重新推导，避免拿旧键图的 index 读新状态。
                self.ctrl = false;
                self.alt = false;
                self.state = Some(xkb::State::new(&keymap));
                true
            }
            None => false,
        }
    }

    /// 同步 modifiers 事件的掩码。wl 协议把三个 mods 掩码定义为「合成器序列化
    /// 给客户端的 xkb mod mask」（bit 位 = 键图 real mod 序号，标准键图 0..7 即
    /// Shift/Lock/Control/Mod1..Mod5，与 wl 文档 shift=0…mod5=7 吻合），直接喂
    /// `update_mask`（winit 同款），不做第二套名字翻译——翻译表就是第二套真相。
    /// group 是有效 layout 序号，按约定放 depressed 位喂入。
    pub fn update_mods(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.update_mask(depressed, latched, locked, group, 0, 0);
        // 权威推导：每次 Modifiers 事件后用 xkb 有效态回读旗标，覆盖 note_key
        // 的键码猜测——虚拟键盘/grab 重建场景键码路径拿不到 Ctrl，只有掩码可信。
        // MOD_INVALID 必须短路：拿无效 index 问 is_active 是未定义行为级别的坑。
        self.ctrl = self.ctrl_mod != xkb::MOD_INVALID
            && state.mod_index_is_active(self.ctrl_mod, xkb::STATE_MODS_EFFECTIVE);
        self.alt = self.alt_mod != xkb::MOD_INVALID
            && state.mod_index_is_active(self.alt_mod, xkb::STATE_MODS_EFFECTIVE);
    }

    /// evdev 键码嗅探（29|97=Ctrl、56|100=Alt 的按下/抬起）：Modifiers 事件到达
    /// 之前的时序兜底。每次 update_mods 都会整体重推导，本函数只负责两帧之间。
    pub fn note_key(&mut self, key: u32, pressed: bool) {
        match key {
            29 | 97 => self.ctrl = pressed,
            56 | 100 => self.alt = pressed,
            _ => {}
        }
    }

    /// Ctrl 旗标只读出口：main.rs 路由/日志统一从这里取，不再自持字段。
    pub fn ctrl(&self) -> bool {
        self.ctrl
    }

    /// Alt 旗标只读出口（同上）。
    pub fn alt(&self) -> bool {
        self.alt
    }

    /// evdev keycode → 当前状态下这个键打出的可打印字符。
    /// 功能键（Esc/Enter/方向键/F1…：utf32 为空或控制字符）和空格返回 None，
    /// 由调用方按 `code` 走引擎——空格必须算功能键：引擎的「空格上屏首选」
    /// 分支匹配的是 `ch.is_none() && code == KEY_SPACE`。
    ///
    /// Ctrl 兜底（第五轮 C-f/C-b/C-h 真机失效的根因修复）：`xkb_state_key_get_utf32`
    /// 在 Control 激活且未被该键消费时按 XKB 规范做 XkbToControl 变换（'f'→0x06，
    /// libxkbcommon state.c::should_do_ctrl_transformation），控制字符被上面的过滤
    /// 挡掉 → 引擎里按字符匹配的 C-b/f/h/n/p/. 分支在真机上永远收不到 Some('f')。
    /// Control 不是换层修饰：`key_get_one_sym` 的键符不受它影响，回退取键符转 utf32。
    /// 白名单只放行 ASCII 字母与 '.'——恰好是引擎 ctrl 组合分支消费的字符集；
    /// Ctrl+数字/Ctrl+标点/Ctrl+回车等组合维持 ch=None 原样转发应用。
    pub fn key_char(&self, evdev: u32) -> Option<char> {
        let state = self.state.as_ref()?;
        // evdev scancode 与 xkb keycode 的固定偏移是 8
        let kc = xkb::Keycode::new(evdev + 8);
        if let Some(ch) = char_from_utf32(state.key_get_utf32(kc)) {
            return Some(ch);
        }
        if self.ctrl_mod == xkb::MOD_INVALID
            || !state.mod_index_is_active(self.ctrl_mod, xkb::STATE_MODS_EFFECTIVE)
        {
            return None;
        }
        let sym = state.key_get_one_sym(kc);
        char_from_utf32(xkb::keysym_to_utf32(sym))
            .filter(|ch| ch.is_ascii_alphabetic() || *ch == '.')
    }
}

/// utf32 → 可打印字符：0（功能键/多键符）与控制字符（含 DEL）不是文本。
fn char_from_utf32(utf32: u32) -> Option<char> {
    let ch = char::from_u32(utf32)?;
    ((ch as u32) > ' ' as u32 && (ch as u32) != 0x7f).then_some(ch)
}
