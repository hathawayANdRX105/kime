impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for AppState {
    fn event(
        state: &mut Self,
        _grab: &ZwpInputMethodKeyboardGrabV2,
        event: ZwpInputMethodKeyboardGrabEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ZwpInputMethodKeyboardGrabEvent::Key {
                key,
                state: key_state,
                ..
            } => {
                if !matches!(key_state, WEnum::Value(KeyState::Pressed)) {
                    return;
                }

                let is_shift = matches!(key, 42 | 54);
                let is_page_key = matches!(key, 12 | 13 | 26 | 27);

                // 字母 a-z 范围 30..=54，Shift 键本身也在这范围内
                // 所以需要排除 Shift 键本身
                let ch = if is_shift {
                    None
                } else {
                    match key {
                        1 => Some('\u{1b}'),
                        30..=38 => Some((b'a' + (key - 30) as u8) as char),
                        44..=53 => Some((b'j' + (key - 44) as u8) as char),
                        11..=20 => Some((b'0' + (key - 11) as u8) as char),
                        57 => Some(' '),
                        28 => Some('\n'),
                        _ if is_page_key => None,
                        _ => return,
                    }
                };

                if let Some(engine) = &mut state.engine {
                    let key_struct = Key {
                        ch,
                        code: key,
                        shift: is_shift,
                        ctrl: false,
                        alt: false,
                    };
                    match engine.key(key_struct) {
                        Outcome::Consumed => {
                            let pe = engine.preedit().to_string();
                            log(&format!("engine consumed: preedit={}", pe));
                            if let Some(im) = &state.input_method {
                                let cursor = pe.len() as i32;
                                im.set_preedit_string(pe, 0, cursor);
                                im.commit(state.im_serial);
                            }
                            if let Some(win) = &mut state.window {
                                let ui: Vec<UiCandidate> = engine
                                    .candidates()
                                    .iter()
                                    .map(|c| UiCandidate {
                                        text: c.text.clone(),
                                        pinyin: c.pinyin.clone(),
                                        freq: c.freq,
                                        ai: c.ai,
                                    })
                                    .collect();
                                if ui.is_empty() {
                                    let _ = win.hide();
                                } else {
                                    let _ =
                                        win.show(&ui, engine.highlight(), engine.preedit());
                                }
                            }
                            if let Some(worker) = &state.llm_worker {
                                worker.request(engine.preedit().split("'").map(String::from).collect());
                            }
                        }
                        Outcome::Commit(text) => {
                            log(&format!("engine commit: {}", text));
                            if let Some(im) = &state.input_method {
                                im.commit_string(text);
                                im.commit(state.im_serial);
                            }
                            if let Some(win) = &mut state.window {
                                let _ = win.hide();
                            }
                        }
                        Outcome::Ignored => {
                            if let Some(c) = ch {
                                if let Some(im) = &state.input_method {
                                    let _ = im.commit_string(c.to_string());
                                    im.commit(state.im_serial);
                                }
                            }
                            log(&format!("engine ignored: ch={:?} passthrough", ch));
                        }
                    }
                    state.try_recv_llm();
                }
            }
            _ => {}
        }
    }
}
