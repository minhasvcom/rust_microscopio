use eframe::egui;
use image::{imageops::FilterType, DynamicImage, ImageBuffer, Rgb, RgbImage};
use nokhwa::{
    pixel_format::RgbFormat,
    utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution},
    Camera,
};
use std::{
    fs::{create_dir_all, File},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

#[derive(Clone, Debug, PartialEq)]
struct InfoCamera {
    nome: String,
    index: CameraIndex,
}

struct MicroscopioApp {
    camera: Arc<Mutex<Option<Camera>>>,
    textura_frame: Option<egui::TextureHandle>,

    // Seleção de Câmeras
    lista_cameras: Vec<InfoCamera>,
    camera_selecionada: Option<InfoCamera>,

    // Ferramentas de Zoom e Navegação
    fator_zoom: f32,
    offset_pan: egui::Vec2,

    // Controles de Processamento de Imagem
    brilho: f32,    // -100.0 a 100.0
    contraste: f32, // -100.0 a 100.0

    // Gravação e Captura
    gravando_video: Arc<AtomicBool>,
    frames_video: Arc<Mutex<Vec<RgbImage>>>,
    tempo_inicio_gravacao: Option<Instant>,
    caminho_salvamento: PathBuf,
    status_mensagem: String,

    // Cache de imagem para salvar snapshots em alta qualidade
    ultimo_frame_processado: Option<RgbImage>,
}

impl MicroscopioApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let pasta_capturas = PathBuf::from("capturas_microscopio");
        let _ = create_dir_all(&pasta_capturas);

        let mut app = Self {
            camera: Arc::new(Mutex::new(None)),
            textura_frame: None,
            lista_cameras: Vec::new(),
            camera_selecionada: None,
            fator_zoom: 1.0,
            offset_pan: egui::Vec2::ZERO,
            brilho: 0.0,
            contraste: 0.0,
            gravando_video: Arc::new(AtomicBool::new(false)),
            frames_video: Arc::new(Mutex::new(Vec::new())),
            tempo_inicio_gravacao: None,
            caminho_salvamento: pasta_capturas,
            status_mensagem: "Detectando câmeras...".to_string(),
            ultimo_frame_processado: None,
        };

        app.atualizar_lista_cameras();
        app
    }

    fn atualizar_lista_cameras(&mut self) {
        self.lista_cameras.clear();
        if let Ok(dispositivos) = nokhwa::query(nokhwa::utils::ApiBackend::Auto) {
            for cam in dispositivos {
                self.lista_cameras.push(InfoCamera {
                    nome: cam.human_name(),
                    index: cam.index().clone(),
                });
            }
        }

        if self.lista_cameras.is_empty() {
            self.status_mensagem = "Nenhuma câmera encontrada! Conecte o microscópio USB.".to_string();
            return;
        }

        let mut conectou = false;
        let lista = self.lista_cameras.clone();
        for cam_info in lista {
            if self.tentar_abrir_camera(&cam_info) {
                conectou = true;
                break;
            }
        }

        if !conectou {
            self.status_mensagem = "Câmeras encontradas, mas indisponíveis no momento.".to_string();
        }
    }

    fn tentar_abrir_camera(&mut self, info_cam: &InfoCamera) -> bool {
        if let Ok(mut guard) = self.camera.lock() {
            *guard = None;
        }

        let resolucoes_desejadas = [
            Resolution::new(1920, 1080), // Full HD
            Resolution::new(1280, 720),  // HD
            Resolution::new(1024, 768),
            Resolution::new(800, 600),
        ];

        let mut camera_criada = None;

        for res in resolucoes_desejadas {
            let format = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(
                CameraFormat::new(res, FrameFormat::MJPEG, 30),
            ));

            if let Ok(cam) = Camera::new(info_cam.index.clone(), format) {
                camera_criada = Some(cam);
                break;
            }
        }

        let mut nova_cam = match camera_criada {
            Some(c) => c,
            None => {
                let format_fallback = RequestedFormat::new::<RgbFormat>(RequestedFormatType::None);
                match Camera::new(info_cam.index.clone(), format_fallback) {
                    Ok(c) => c,
                    Err(_) => return false,
                }
            }
        };

        if nova_cam.open_stream().is_ok() {
            let res_atual = nova_cam.camera_format().resolution();
            if let Ok(mut guard) = self.camera.lock() {
                *guard = Some(nova_cam);
            }
            self.status_mensagem = format!(
                "Conectado: {} [{}x{}]",
                info_cam.nome,
                res_atual.width(),
                res_atual.height()
            );
            self.camera_selecionada = Some(info_cam.clone());
            true
        } else {
            false
        }
    }

    fn trocar_camera(&mut self, info_cam: InfoCamera) {
        if !self.tentar_abrir_camera(&info_cam) {
            self.status_mensagem = format!("Não foi possível abrir a câmera: {}", info_cam.nome);
        }
    }

    fn tirar_foto(&mut self) {
        if let Some(ref img_bytes) = self.ultimo_frame_processado {
            let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let nome_arquivo = self.caminho_salvamento.join(format!("foto_{}.png", timestamp));

            if img_bytes.save(&nome_arquivo).is_ok() {
                self.status_mensagem = format!("Foto salva: {:?}", nome_arquivo.file_name().unwrap());
            } else {
                self.status_mensagem = "Erro ao salvar foto!".to_string();
            }
        } else {
            self.status_mensagem = "Nenhum frame ativo para capturar.".to_string();
        }
    }

    fn alternar_gravacao(&mut self) {
        let estado_atual = self.gravando_video.load(Ordering::SeqCst);

        if estado_atual {
            self.gravando_video.store(false, Ordering::SeqCst);
            self.status_mensagem = "Salvando gravação em GIF... Aguarde.".to_string();

            let frames_para_salvar = {
                let mut guard = self.frames_video.lock().unwrap();
                std::mem::take(&mut *guard)
            };

            let caminho_pasta = self.caminho_salvamento.clone();

            std::thread::spawn(move || {
                let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let caminho_video = caminho_pasta.join(format!("gravacao_{}.gif", timestamp));

                if let Some(primeiro) = frames_para_salvar.first() {
                    let width = primeiro.width() as u16;
                    let height = primeiro.height() as u16;

                    if let Ok(file) = File::create(&caminho_video) {
                        let mut encoder = gif::Encoder::new(file, width, height, &[]).unwrap();
                        let _ = encoder.set_repeat(gif::Repeat::Infinite);

                        for frame_rgb in frames_para_salvar {
                            let mut frame_gif = gif::Frame::from_rgb(width, height, &frame_rgb.into_raw());
                            frame_gif.delay = 10;
                            let _ = encoder.write_frame(&frame_gif);
                        }
                    }
                }
            });

            self.tempo_inicio_gravacao = None;
            self.status_mensagem = "Gravação em GIF salva com sucesso!".to_string();
        } else {
            self.frames_video.lock().unwrap().clear();
            self.gravando_video.store(true, Ordering::SeqCst);
            self.tempo_inicio_gravacao = Some(Instant::now());
            self.status_mensagem = "Gravando...".to_string();
        }
    }
}

impl eframe::App for MicroscopioApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Atribuição de Atalhos via Teclado (Zoom, Foto e Gravação)
        ctx.input(|i| {
            // Atalhos de Zoom
            if i.key_pressed(egui::Key::ArrowRight) {
                self.fator_zoom = (self.fator_zoom + 0.2).min(10.0);
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.fator_zoom = (self.fator_zoom - 0.2).max(1.0);
                if self.fator_zoom == 1.0 {
                    self.offset_pan = egui::Vec2::ZERO;
                }
            }
            if i.key_pressed(egui::Key::R) {
                self.fator_zoom = 1.0;
                self.offset_pan = egui::Vec2::ZERO;
            }

            // Atalho F -> Tirar Foto
            if i.key_pressed(egui::Key::F) {
                self.tirar_foto();
            }

            // Atalho G -> Iniciar/Parar Gravação
            if i.key_pressed(egui::Key::G) {
                self.alternar_gravacao();
            }
        });

        // 1. Processamento e Captura
        if let Ok(mut cam_guard) = self.camera.lock() {
            if let Some(ref mut cam) = *cam_guard {
                if let Ok(frame) = cam.frame() {
                    if let Ok(rgb_buffer) = frame.decode_image::<RgbFormat>() {
                        let largura = rgb_buffer.width();
                        let altura = rgb_buffer.height();

                        let mut img_original = DynamicImage::ImageRgb8(
                            ImageBuffer::<Rgb<u8>, _>::from_raw(largura, altura, rgb_buffer.to_vec())
                                .unwrap(),
                        );

                        // Aplicação de Brilho e Contraste
                        if self.brilho != 0.0 || self.contraste != 0.0 {
                            img_original = img_original.adjust_contrast(self.contraste);
                            img_original = img_original.brighten(self.brilho as i32);
                        }

                        // Processamento do Zoom
                        let img_processada = if self.fator_zoom > 1.0 {
                            let nova_largura = ((largura as f32 / self.fator_zoom) as u32).max(1);
                            let nova_altura = ((altura as f32 / self.fator_zoom) as u32).max(1);

                            let max_offset_x = ((largura.saturating_sub(nova_largura)) / 2) as f32;
                            let max_offset_y = ((altura.saturating_sub(nova_altura)) / 2) as f32;

                            let base_x = (largura.saturating_sub(nova_largura)) / 2;
                            let base_y = (altura.saturating_sub(nova_altura)) / 2;

                            let center_x = (base_x as f32 - self.offset_pan.x)
                                .clamp(0.0, (max_offset_x * 2.0).max(0.0)) as u32;
                            let center_y = (base_y as f32 - self.offset_pan.y)
                                .clamp(0.0, (max_offset_y * 2.0).max(0.0)) as u32;

                            let crop_w = nova_largura.min(largura.saturating_sub(center_x));
                            let crop_h = nova_altura.min(altura.saturating_sub(center_y));

                            if crop_w > 0 && crop_h > 0 {
                                let recortada = img_original.crop_imm(center_x, center_y, crop_w, crop_h);
                                recortada.resize_exact(largura, altura, FilterType::CatmullRom)
                            } else {
                                img_original
                            }
                        } else {
                            img_original
                        };

                        let rgb_frame = img_processada.to_rgb8();
                        let (w_final, h_final) = (rgb_frame.width() as usize, rgb_frame.height() as usize);

                        if self.gravando_video.load(Ordering::SeqCst) {
                            self.frames_video.lock().unwrap().push(rgb_frame.clone());
                        }

                        let color_image = egui::ColorImage::from_rgb(
                            [w_final, h_final],
                            &rgb_frame,
                        );

                        self.ultimo_frame_processado = Some(rgb_frame);
                        self.textura_frame = Some(ctx.load_texture(
                            "camera_frame",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }

        // 2. GUI
        egui::SidePanel::left("controles").show(ctx, |ui| {
            ui.heading("🔬 Microscópio USB");
            ui.separator();

            // Seleção de Câmeras
            ui.collapsing("📷 Dispositivo de Vídeo", |ui| {
                ui.horizontal(|ui| {
                    if ui.button("🔄").on_hover_text("Atualizar Lista").clicked() {
                        self.atualizar_lista_cameras();
                    }

                    let mut selecionada_agora = self.camera_selecionada.clone();
                    let rotulo = selecionada_agora
                        .as_ref()
                        .map(|c| c.nome.as_str())
                        .unwrap_or("Selecione...");

                    let mut camera_para_trocar = None;

                    egui::ComboBox::from_label("Câmera")
                        .selected_text(rotulo)
                        .show_ui(ui, |ui| {
                            for cam_info in &self.lista_cameras {
                                if ui
                                    .selectable_value(
                                        &mut selecionada_agora,
                                        Some(cam_info.clone()),
                                        &cam_info.nome,
                                    )
                                    .clicked()
                                {
                                    camera_para_trocar = Some(cam_info.clone());
                                }
                            }
                        });

                    if let Some(nova_cam) = camera_para_trocar {
                        self.trocar_camera(nova_cam);
                    }
                });
            });

            ui.add_space(8.0);

            // Zoom
            ui.collapsing("🔍 Zoom Digital", |ui| {
                ui.label(format!("Nível de Zoom: {:.1}x", self.fator_zoom));
                ui.add(egui::Slider::new(&mut self.fator_zoom, 1.0..=10.0).step_by(0.1));

                ui.label("💡 Atalhos Teclado:");
                ui.label("• Seta Direita (→): Zoom In");
                ui.label("• Seta Esquerda (←): Zoom Out");
                ui.label("• Tecla R: Resetar Zoom");
                ui.label("• Tecla F: Tirar Foto");
                ui.label("• Tecla G: Iniciar/Parar Gravação");

                if ui.button("Resetar Zoom (R)").clicked() {
                    self.fator_zoom = 1.0;
                    self.offset_pan = egui::Vec2::ZERO;
                }
            });

            ui.add_space(8.0);

            // Filtros
            ui.collapsing("🎛️ Filtros de Inspeção", |ui| {
                ui.label("Brilho:");
                ui.add(egui::Slider::new(&mut self.brilho, -100.0..=100.0));

                ui.label("Contraste:");
                ui.add(egui::Slider::new(&mut self.contraste, -100.0..=100.0));

                if ui.button("Resetar Filtros").clicked() {
                    self.brilho = 0.0;
                    self.contraste = 0.0;
                }
            });

            ui.add_space(12.0);
            ui.separator();

            // Captura
            ui.heading("📸 Capturas");

            if ui.button("📷 Tirar Foto (F)").clicked() {
                self.tirar_foto();
            }

            let esta_gravando = self.gravando_video.load(Ordering::SeqCst);
            let btn_text = if esta_gravando { "⏹️ Parar Gravação (G)" } else { "🎥 Gravador GIF (G)" };

            if ui.button(btn_text).clicked() {
                self.alternar_gravacao();
            }

            if esta_gravando {
                if let Some(inicio) = self.tempo_inicio_gravacao {
                    let decorrido = inicio.elapsed().as_secs();
                    ui.colored_label(
                        egui::Color32::RED,
                        format!("🔴 Gravando: {:02}:{:02}", decorrido / 60, decorrido % 60),
                    );
                }
            }

            ui.add_space(15.0);
            ui.separator();

            if ui.button("📂 Abrir Pasta").clicked() {
                let _ = open::that(&self.caminho_salvamento);
            }
        });

        // Barra inferior de status
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Status:");
                ui.colored_label(egui::Color32::LIGHT_BLUE, &self.status_mensagem);
            });
        });

        // Área Central
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(ref textura) = self.textura_frame {
                let resposta = ui.image((textura.id(), ui.available_size()));

                if self.fator_zoom > 1.0 && resposta.dragged() {
                    self.offset_pan += resposta.drag_delta();
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                    ui.label("Selecione o microscópio no menu lateral...");
                });
            }
        });

        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let opcoes = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_title("Microscópio Digital USB - Inspeção Técnica"),
        ..Default::default()
    };

    eframe::run_native(
        "Microscópio Digital USB",
        opcoes,
        Box::new(|cc| Ok(Box::new(MicroscopioApp::new(cc)))),
    )
}
