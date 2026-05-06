// this module is only used by state.rs
// load a .glb glTF 2.0 file and format the data
// on both native and wasm, loading is at runtime
// this allows the glTF file to change without having to rebuild the WASM

pub const ATLAS_SIZE: i32 = 8192; // 8192 is the default of wgpu::limits::max_texture_dimension_2d 
// TODO: set atlas size dynamically; if one atlas is enough, use a smaller atlas size; if multiple are needed, use 8192
// TODO: fix transparency in textures; parsing is done correctly, but shader is not correct. also, in gltf::material, parse alpha_mode and alpha_cutoff to do this properly
// TODO: implement gltf::material double_sided and do backface culling conditionally
// TODO: add parsing of emissive textures

#[derive(Debug)]
pub struct Scene {
    pub geometries: Vec<GpuTriangleGeometry>,
    pub attributes: Vec<GpuTriangleAttribute>,
    pub materials: Vec<GpuMaterial>,
    pub bvh_nodes: Vec<GpuBvhNode>,
    pub texture_atlases: Vec<Vec<u8>>,
    camera: Camera,
}

#[derive(Debug)]
struct Camera {
    position: glam::Vec3, // camera position x, y, z in world space
    // -Z into the screen, +Y up the screen, +X to the right of the screen
    // focus distance is not needed since this is a pinhole camera; focus distance is implicitly 1
    fov_y: f32,        // vertical fov in degrees
    aspect_ratio: f32, // width/height of image plane
    yaw: f32,
    pitch: f32,
    movement_speeds: [f32; 2], // units per second
}

// GpuTriangleGeometry and GpuTriangleAttribute are separate structs the shader fetches vertices much more frequently than normals
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)] // Clone and Copy are required by Pod
#[cfg_attr(
    all(feature = "testing", not(target_arch = "wasm32")),
    derive(serde::Serialize)
)]
pub struct GpuTriangleGeometry {
    // bytemuck::Pod requires alignment without implicit padding
    // although glam::Vec3A is 16 byte aligned, it has padding
    // WebGPU requirements: https://www.w3.org/TR/WGSL/#alignment-and-size
    p0: glam::Vec3,
    p1: glam::Vec3,
    p2: glam::Vec3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[cfg_attr(
    all(feature = "testing", not(target_arch = "wasm32")),
    derive(serde::Serialize)
)]
pub struct GpuTriangleAttribute {
    // points to the index of a GpuMaterial
    // meshes are not passed to the GPU; instead, individual triangles themselves are passed
    // each triangle thus has a material index, which indicates which mesh the triangle is from
    index: u32,
    n0: glam::Vec3,
    n1: glam::Vec3,
    n2: glam::Vec3,
    uv0: glam::Vec2,
    uv1: glam::Vec2,
    uv2: glam::Vec2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[cfg_attr(
    all(feature = "testing", not(target_arch = "wasm32")),
    derive(serde::Serialize)
)]
pub struct GpuMaterial {
    base_color_factor: glam::Vec4,
    base_color_uv: glam::Vec4, // uvs have offset_x, offset_y, scale_x, scale_y
    normal_uv: glam::Vec4,
    metallic_roughness_uv: glam::Vec4,
    emissive: glam::Vec4, // emissive color in rgb, then strength in the 4th value
    base_color_tex_layer: i32, // index the array of texture atlases for this texture; -1 if no texture
    normal_scale: f32,
    normal_tex_layer: i32,
    metallic_roughness_tex_layer: i32,
    metallic_factor: f32,
    roughness_factor: f32,
    pad: [i32; 2],
}

// if prim_count == 0, the node is an internal node, and left_first is the index of the left child node in the Vec<GpuBvhNode>
// the right child is guaranteed to be immediately after left_first at left_first + 1
// if prim_count > 0, the node is a leaf, and left_first is instead the starting offset into Vec<GpuTriangleGeometry>, where the primitives contained in this leaf node are at indices left_first to left_first + prim_count - 1
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuBvhNode {
    aabb_min: glam::Vec3,
    left_first: u32,
    aabb_max: glam::Vec3,
    prim_count: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
/* although the 4th value of each Vec4 is unused, if having Vec3 with matching WGSL:
struct GpuCamera {
    data: array<f32, 12>,
};
does not work for UNIFORM buffers
Error: Global variable [5] 'camera' is invalid: Alignment requirements for address space Uniform are not met by [10]The array stride 4 is not a multiple of the required alignment 16naga(15)
Even putting #[repr(C, align(16))] on this struct does not work
*/
pub struct GpuCamera {
    position: glam::Vec4,          // camera position, same as in Camera
    lower_left_corner: glam::Vec4, // lower-left pixel coordinate of image plane in world space
    horizontal: glam::Vec4,        // vector that spans the full x of image plane in world space
    vertical: glam::Vec4,          // vector that spans the full y of image plane in world space
}

// internal BVH helper structs
#[derive(Clone, Copy, Debug)]
struct Aabb {
    min: glam::Vec3,
    max: glam::Vec3,
}
impl Aabb {
    const fn new() -> Self {
        Self {
            min: glam::Vec3::splat(f32::INFINITY),
            max: glam::Vec3::splat(f32::NEG_INFINITY),
        }
    }
    fn grow(&mut self, p: glam::Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    #[allow(clippy::use_self)]
    fn union(&mut self, other: &Aabb) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }
    fn area(&self) -> f32 {
        let e = (self.max - self.min).max(glam::Vec3::ZERO);
        e.z.mul_add(e.x, e.x.mul_add(e.y, e.y * e.z)) // e.x * e.y + e.y * e.z + e.z * e.x, but using mul_add for better precision and performance
    }
}
#[derive(Clone, Copy)]
struct PrimitiveInfo {
    aabb: Aabb,
    centroid: glam::Vec3,
}

// helper to convert image into RGBA8
// most textures from most models are 8 bit; higher bit depth textures are uncommon
// is_srgb for converting base color textures from sRGB to linear; only base color textures are sRGB in glTF, which needs conversion
// https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html#metallic-roughness-material
fn convert_to_rgba8(image: &gltf::image::Data, is_srgb: bool, srgb_lut: &[u8; 256]) -> Vec<u8> {
    use gltf::image::Format;

    #[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
    log::info!("Converting image with format {:?} to RGBA8", image.format);

    let mut rgba = match image.format {
        Format::R8 => image.pixels.iter().flat_map(|&r| [r, r, r, 255]).collect(),
        Format::R8G8 => image
            .pixels
            .chunks_exact(2)
            .flat_map(|rg| [rg[0], rg[1], 0, 255])
            .collect(),
        Format::R8G8B8 => image
            .pixels
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        Format::R8G8B8A8 => image.pixels.clone(),

        Format::R16 => image
            .pixels
            .chunks_exact(2)
            .flat_map(|r| {
                let v = (u16::from_le_bytes([r[0], r[1]]) >> 8) as u8;
                [v, v, v, 255]
            })
            .collect(),
        Format::R16G16 => image
            .pixels
            .chunks_exact(4)
            .flat_map(|rg| {
                let r = (u16::from_le_bytes([rg[0], rg[1]]) >> 8) as u8;
                let g = (u16::from_le_bytes([rg[2], rg[3]]) >> 8) as u8;
                [r, r, r, g]
            })
            .collect(),
        Format::R16G16B16 => image
            .pixels
            .chunks_exact(6)
            .flat_map(|rgb| {
                let r = (u16::from_le_bytes([rgb[0], rgb[1]]) >> 8) as u8;
                let g = (u16::from_le_bytes([rgb[2], rgb[3]]) >> 8) as u8;
                let b = (u16::from_le_bytes([rgb[4], rgb[5]]) >> 8) as u8;
                [r, g, b, 255]
            })
            .collect(),
        Format::R16G16B16A16 => image
            .pixels
            .chunks_exact(8)
            .flat_map(|rgba| {
                let r = (u16::from_le_bytes([rgba[0], rgba[1]]) >> 8) as u8;
                let g = (u16::from_le_bytes([rgba[2], rgba[3]]) >> 8) as u8;
                let b = (u16::from_le_bytes([rgba[4], rgba[5]]) >> 8) as u8;
                let a = (u16::from_le_bytes([rgba[6], rgba[7]]) >> 8) as u8;
                [r, g, b, a]
            })
            .collect(),
        _ => {
            log::warn!(
                "Unsupported image format {:?}, using fallback opaque white texture",
                image.format
            );
            vec![255; (image.width * image.height * 4) as usize]
        }
    };

    // sRGB -> linear conversion mapping
    if is_srgb {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[0] = srgb_lut[pixel[0] as usize];
            pixel[1] = srgb_lut[pixel[1] as usize];
            pixel[2] = srgb_lut[pixel[2] as usize];
            // the alpha channel is strictly linear per the glTF spec, so don't modify pixel[3]
        }
    }

    rgba
}

// helper to fetch mapping bounds from dictionary
fn get_layer_and_uv(
    img_idx_opt: Option<usize>,
    image_uvs: &std::collections::HashMap<usize, (i32, glam::Vec4)>,
) -> (i32, glam::Vec4) {
    img_idx_opt
        .and_then(|idx| image_uvs.get(&idx).copied())
        .unwrap_or((-1, glam::Vec4::ZERO)) // return -1 layer if no texture attached
}

// private helper function used in new()
// not included directly inside new because conditional compilation with variable scopes would get messy
#[allow(clippy::unused_async)]
async fn load_gltf_bytes(path: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Ok(std::fs::read(path)?)
    }

    // on web, the glTF file is fetched over HTTP
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast; // trait for dyn_into()

        let response: web_sys::Response = wasm_bindgen_futures::JsFuture::from(
            web_sys::window()
                .ok_or("no window object")? // ok_or() converts Option<> to Result<> with error message
                .fetch_with_str(path),
        )
        .await
        .map_err(|e| format!("{:?}", e))?
        .dyn_into()
        .map_err(|e| format!("{:?}", e))?;

        if !response.ok() {
            return Err(format!("network error: status {}", response.status()).into());
        }

        let u8_array = js_sys::Uint8Array::new(
            &wasm_bindgen_futures::JsFuture::from(
                response.array_buffer().map_err(|e| format!("{:?}", e))?,
            )
            .await
            .map_err(|e| format!("{:?}", e))?,
        );
        let mut bytes = vec![0u8; u8_array.length() as usize];
        u8_array.copy_to(&mut bytes[..]);
        Ok(bytes)
    }
}

impl Scene {
    pub async fn new(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let (document, buffers, images) = gltf::import_slice(load_gltf_bytes(path).await?)?;

        let mut geometries = Vec::new();
        let mut attributes = Vec::new();
        let mut materials = Vec::new();
        let mut prim_infos = Vec::new();

        // the 2 extensions enabled are KHR_materials_emissive_strength and KHR_materials_specular
        // in Blender, with principled BSDF, if emission strength is larger than 1.0, KHR_materials_emissive_strength will automatically be used in the exported glTF file
        document.extensions_used().for_each(|s| {
            log::info!("glTF includes extension: {s}");
        });

        // precompute an 8-bit sRGB to linear lookup table to avoid expensive math per-pixel
        let mut srgb_to_linear_lut = [0u8; 256];
        for (i, c) in srgb_to_linear_lut.iter_mut().enumerate() {
            let f = i as f32 / 255.0;
            let linear = if f <= 0.04045 {
                f / 12.92
            } else {
                ((f + 0.055) / 1.055).powf(2.4)
            };
            *c = (linear * 255.0).round() as u8;
        }

        // collect indices of images designated as sRGB (base color or emissive textures) per glTF spec
        let mut srgb_images = std::collections::HashSet::new();
        for material in document.materials() {
            if let Some(tex) = material.pbr_metallic_roughness().base_color_texture() {
                srgb_images.insert(tex.texture().source().index());
            }
            if let Some(tex) = material.emissive_texture() {
                srgb_images.insert(tex.texture().source().index());
            }
        }

        struct AtlasLayer {
            allocator: guillotiere::AtlasAllocator,
            pixels: Vec<u8>,
        }
        let mut atlases = vec![AtlasLayer {
            allocator: guillotiere::AtlasAllocator::new(guillotiere::size2(ATLAS_SIZE, ATLAS_SIZE)),
            pixels: vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize], // * 4 for RGBA
        }];

        let mut image_uvs = std::collections::HashMap::new();

        // loop through each image
        // if there are no images, this loop is skipped, and atlases just contains one empty ATLAS_SIZE RGBA texture, which is fine since the shader will check if the layer index is -1 and skip texturing in that case
        for (img_idx, image) in images.iter().enumerate() {
            let rgba = convert_to_rgba8(image, srgb_images.contains(&img_idx), &srgb_to_linear_lut);
            let size = guillotiere::size2(image.width.cast_signed(), image.height.cast_signed());
            let mut allocation = None;
            let mut layer_idx = 0;

            // try to find a layer that has enough space
            for (i, layer) in atlases.iter_mut().enumerate() {
                if let Some(alloc) = layer.allocator.allocate(size) {
                    allocation = Some(alloc);
                    layer_idx = i;
                    break;
                }
            }

            // if all atlases are full, create a new atlas
            if allocation.is_none() {
                let mut new_layer = AtlasLayer {
                    allocator: guillotiere::AtlasAllocator::new(guillotiere::size2(
                        ATLAS_SIZE, ATLAS_SIZE,
                    )),
                    pixels: vec![0; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize],
                };
                allocation = new_layer.allocator.allocate(size);
                layer_idx = atlases.len();
                atlases.push(new_layer);
            }

            let alloc = allocation.expect("Single image is larger than ATLAS_SIZE atlas.");
            let rect = alloc.rectangle;

            // blit the image into the atlas buffer
            let layer = &mut atlases[layer_idx];
            for y in 0..image.height.cast_signed() {
                let src_start = (y * image.width.cast_signed() * 4) as usize;
                let src_end = src_start + (image.width.cast_signed() * 4) as usize;

                let dst_start = ((rect.min.y + y) * ATLAS_SIZE * 4 + rect.min.x * 4) as usize;
                let dst_end = dst_start + (image.width.cast_signed() * 4) as usize;

                layer.pixels[dst_start..dst_end].copy_from_slice(&rgba[src_start..src_end]);
            }

            // calculate scale and offset in the 0.0 -> 1.0 range based on the atlas
            let uv_offset_scale = glam::Vec4::new(
                rect.min.x as f32 / ATLAS_SIZE as f32,   // offset x
                rect.min.y as f32 / ATLAS_SIZE as f32,   // offset y
                image.width as f32 / ATLAS_SIZE as f32,  // scale x
                image.height as f32 / ATLAS_SIZE as f32, // scale y
            );

            #[allow(clippy::cast_possible_wrap)]
            image_uvs.insert(img_idx, (layer_idx as i32, uv_offset_scale));
        }

        #[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
        {
            // save parsed images for testing
            for (img_idx, image) in images.iter().enumerate() {
                let rgba =
                    convert_to_rgba8(image, srgb_images.contains(&img_idx), &srgb_to_linear_lut);
                Self::save_rgba_image(
                    &format!("parsed_image_{img_idx}.png"),
                    &rgba,
                    image.width,
                    image.height,
                );
            }
        }

        // recursive helper to process all nodes and their children
        fn process_node_recursive(
            node: &gltf::scene::Node,
            parent_mat: glam::Mat4,
            buffers: &[gltf::buffer::Data],
            image_uvs: &std::collections::HashMap<usize, (i32, glam::Vec4)>,
            geometries: &mut Vec<GpuTriangleGeometry>,
            attributes: &mut Vec<GpuTriangleAttribute>,
            materials: &mut Vec<GpuMaterial>,
            prim_infos: &mut Vec<PrimitiveInfo>,
        ) {
            // compute this node's transform matrix
            let local_mat = match node.transform() {
                gltf::scene::Transform::Matrix { matrix } => {
                    glam::Mat4::from_cols_array_2d(&matrix)
                }
                gltf::scene::Transform::Decomposed {
                    translation,
                    rotation,
                    scale,
                } => {
                    glam::Mat4::from_translation(glam::Vec3::from(translation))
                        * glam::Mat4::from_quat(glam::Quat::from_array(rotation))
                        * glam::Mat4::from_scale(glam::Vec3::from(scale))
                }
            };

            let model_mat = parent_mat * local_mat;
            let normal_mat = model_mat.inverse().transpose();

            // get the mesh data if this node references a mesh
            if let Some(mesh) = node.mesh() {
                // primitives are the only useful data in a gltf::Mesh for this renderer
                // usually there's only one primitive for a mesh
                for primitive in mesh.primitives() {
                    // only process the primitive if it's triangles
                    if primitive.mode() == gltf::mesh::Mode::Triangles {
                        // a reader can read the data of a mesh primitive
                        let reader =
                            primitive.reader(|buffer| Some(&buffers[buffer.index()].0[..]));

                        let positions: Vec<glam::Vec3> = match reader.read_positions() {
                            // type annotation necessary
                            Some(it) => it.map(glam::Vec3::from_array).collect(), // collect the iterator
                            None => continue, // if there are no positions (vertex positions), skip this primitive
                        };

                        let normals = reader.read_normals().map_or_else(
                            || vec![glam::Vec3::Y; positions.len()],
                            |it| it.map(glam::Vec3::from_array).collect(),
                        );

                        let tex_coords: Vec<glam::Vec2> = reader.read_tex_coords(0).map_or_else(
                            || vec![glam::Vec2::ZERO; positions.len()],
                            |read_tex_coords| {
                                read_tex_coords
                                    .into_f32()
                                    .map(glam::Vec2::from_array)
                                    .collect()
                            },
                        ); // if there are no tex coords, use (0, 0) for all vertices

                        let indices: Vec<u32> = reader.read_indices().map_or_else(
                            || (0u32..positions.len() as u32).collect(),
                            |read_indices| read_indices.into_u32().collect(),
                        );

                        let material_index = materials.len() as u32;

                        let mat = primitive.material();
                        let pbr = mat.pbr_metallic_roughness();

                        let (base_color_tex_layer, base_color_uv) = get_layer_and_uv(
                            pbr.base_color_texture()
                                .map(|t| t.texture().source().index()), // this index should match the image's index from images.iter().enumerate(), which is a key in image_uvs
                            image_uvs,
                        );

                        let (metallic_roughness_tex_layer, metallic_roughness_uv) =
                            get_layer_and_uv(
                                pbr.metallic_roughness_texture()
                                    .map(|t| t.texture().source().index()),
                                image_uvs,
                            );

                        let norm_tex = mat.normal_texture();
                        let (normal_tex_layer, normal_uv) = get_layer_and_uv(
                            norm_tex.as_ref().map(|t| t.texture().source().index()),
                            image_uvs,
                        );

                        materials.push(GpuMaterial {
                            base_color_factor: glam::Vec4::from(pbr.base_color_factor()),
                            base_color_uv,
                            normal_uv,
                            metallic_roughness_uv,
                            emissive: glam::Vec4::new(
                                mat.emissive_factor()[0],
                                mat.emissive_factor()[1],
                                mat.emissive_factor()[2],
                                mat.emissive_strength().unwrap_or_default(),
                            ),
                            base_color_tex_layer,
                            normal_scale: norm_tex.map_or(1.0, |t| t.scale()),
                            normal_tex_layer,
                            metallic_roughness_tex_layer,
                            metallic_factor: pbr.metallic_factor(),
                            roughness_factor: pbr.roughness_factor(),
                            pad: [0; 2],
                        });

                        for chunk in indices.chunks_exact(3) {
                            let i0 = chunk[0] as usize;
                            let i1 = chunk[1] as usize;
                            let i2 = chunk[2] as usize;

                            let p0 = model_mat.transform_point3(positions[i0]);
                            let p1 = model_mat.transform_point3(positions[i1]);
                            let p2 = model_mat.transform_point3(positions[i2]);

                            // calculate AABB and centroid for the BVH builder
                            prim_infos.push(PrimitiveInfo {
                                aabb: Aabb {
                                    min: p0.min(p1).min(p2),
                                    max: p0.max(p1).max(p2),
                                },
                                centroid: (p0 + p1 + p2) / 3.0,
                            });

                            geometries.push(GpuTriangleGeometry { p0, p1, p2 });

                            attributes.push(GpuTriangleAttribute {
                                index: material_index,
                                n0: normal_mat.transform_vector3(normals[i0]).normalize(),
                                n1: normal_mat.transform_vector3(normals[i1]).normalize(),
                                n2: normal_mat.transform_vector3(normals[i2]).normalize(),
                                uv0: tex_coords[i0],
                                uv1: tex_coords[i1],
                                uv2: tex_coords[i2],
                            });
                        }
                    } else {
                        log::info!(
                            "Mesh mode is not triangles, skipping primitive with mode {:?}",
                            primitive.mode()
                        );
                    }
                }
            }

            // recursively process all child nodes
            for child in node.children() {
                process_node_recursive(
                    &child, model_mat, buffers, image_uvs, geometries, attributes, materials,
                    prim_infos,
                );
            }
        }

        // collect meshes referenced by nodes (including all children)
        let root_nodes = document.default_scene().map_or_else(
            || document.nodes().collect::<Vec<_>>(),
            |scene| scene.nodes().collect::<Vec<_>>(),
        );

        for node in root_nodes {
            process_node_recursive(
                &node,
                glam::Mat4::IDENTITY,
                &buffers,
                &image_uvs,
                &mut geometries,
                &mut attributes,
                &mut materials,
                &mut prim_infos,
            );
        }

        let mut bvh_nodes = vec![GpuBvhNode {
            aabb_min: glam::Vec3::ZERO,
            left_first: 0,
            aabb_max: glam::Vec3::ZERO,
            prim_count: 0,
        }];

        log::info!(
            "Loaded scene with {} triangles, {} materials, {} texture atlases; starting BVH construction",
            geometries.len(),
            materials.len(),
            atlases.len(),
        );

        let bvh_construction_start = web_time::Instant::now();

        if !prim_infos.is_empty() {
            let prim_count = prim_infos.len();
            Self::update_node_bounds(0, &mut bvh_nodes, &prim_infos, 0, prim_count);
            Self::subdivide(
                0,
                &mut bvh_nodes,
                &mut prim_infos,
                &mut geometries,
                &mut attributes,
                0,
                prim_count,
            );
        }

        log::info!(
            "Finished BVH construction with {} nodes, took {:?}",
            bvh_nodes.len(),
            bvh_construction_start.elapsed()
        );

        // calculate a decent starting camera position from scene bounds
        let root_aabb = bvh_nodes[0];
        let scene_center = (root_aabb.aabb_min + root_aabb.aabb_max) * 0.5;
        let scene_height = root_aabb.aabb_max.y - root_aabb.aabb_min.y;

        #[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
        {
            // save atlases for testing
            for (idx, atlas) in atlases.iter().enumerate() {
                Self::save_rgba_image(
                    &format!("atlas_{idx}.png"),
                    &atlas.pixels,
                    ATLAS_SIZE as u32,
                    ATLAS_SIZE as u32,
                );
            }
        }

        let scene = Self {
            geometries,
            attributes,
            materials,
            bvh_nodes,
            texture_atlases: atlases.into_iter().map(|layer| layer.pixels).collect(), // map out just the pixels from the AtlasLayer structs
            camera: Camera {
                position: glam::Vec3::new(
                    scene_center.x,
                    scene_height.mul_add(0.01, root_aabb.aabb_max.y), // position the camera above the top of the scene bounds
                    scene_center.z,
                ),
                fov_y: 90.0,
                aspect_ratio: 1.0,
                yaw: 0.0,
                pitch: 0.0,
                movement_speeds: [(root_aabb.aabb_max.x - root_aabb.aabb_min.x)
                    .max(root_aabb.aabb_max.z - root_aabb.aabb_min.z)
                    * 0.15; 2],
            },
        };

        #[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
        scene.save_scene_data_json();

        Ok(scene)
    }

    // updates bounding box for a given node based on triangles it spans
    fn update_node_bounds(
        node_idx: usize,
        nodes: &mut [GpuBvhNode],
        prim_infos: &[PrimitiveInfo],
        start: usize,
        end: usize,
    ) {
        let mut aabb = Aabb::new();
        for info in &prim_infos[start..end] {
            aabb.union(&info.aabb);
        }
        nodes[node_idx].aabb_min = aabb.min;
        nodes[node_idx].aabb_max = aabb.max;
    }
    // binning SAH sub-divider
    fn subdivide(
        node_idx: usize,
        nodes: &mut Vec<GpuBvhNode>,
        prim_infos: &mut [PrimitiveInfo],
        geometries: &mut [GpuTriangleGeometry],
        attributes: &mut [GpuTriangleAttribute],
        start: usize,
        end: usize,
    ) {
        let prim_count = end - start;
        // if there are 2 or fewer triangles, make this node a leaf; otherwise, keep subdividing
        if prim_count <= 2 {
            nodes[node_idx].left_first = start as u32;
            nodes[node_idx].prim_count = prim_count as u32;
            return;
        }

        // split based on not the edges of triangles; calculate bounding box that encapsulates only the centroids
        let mut centroid_bounds = Aabb::new();
        for info in &prim_infos[start..end] {
            centroid_bounds.grow(info.centroid);
        }

        const BINS: usize = 8;
        let mut best_axis = 0;
        let mut best_split = 0;
        let mut best_cost = f32::MAX;

        for axis in 0..3 {
            // for axis in x, y, z
            let bounds_min = centroid_bounds.min[axis];
            let bounds_max = centroid_bounds.max[axis];
            #[allow(clippy::float_cmp)] // there should be no precision drift here
            if bounds_min == bounds_max {
                continue;
            } // all primitive centroids are overlapping on this axis

            let scale = BINS as f32 / (bounds_max - bounds_min);

            #[derive(Clone, Copy)]
            struct Bin {
                count: u32,
                bounds: Aabb,
            }
            let mut bins = [Bin {
                count: 0,
                bounds: Aabb::new(),
            }; BINS];

            for info in &prim_infos[start..end] {
                let centroid = info.centroid[axis];
                let mut bin_idx = ((centroid - bounds_min) * scale) as usize;
                bin_idx = bin_idx.min(BINS - 1);
                bins[bin_idx].count += 1;
                bins[bin_idx].bounds.union(&info.aabb);
            }

            let mut left_area = [0.0; BINS - 1];
            let mut left_count = [0; BINS - 1];
            let mut right_area = [0.0; BINS - 1];
            let mut right_count = [0; BINS - 1];

            let mut left_box = Aabb::new();
            let mut left_sum = 0;
            for i in 0..BINS - 1 {
                left_sum += bins[i].count;
                left_box.union(&bins[i].bounds);
                left_count[i] = left_sum;
                left_area[i] = left_box.area();
            }

            let mut right_box = Aabb::new();
            let mut right_sum = 0;
            for i in (1..BINS).rev() {
                right_sum += bins[i].count;
                right_box.union(&bins[i].bounds);
                right_count[i - 1] = right_sum;
                right_area[i - 1] = right_box.area();
            }

            for i in 0..BINS - 1 {
                let cost = (left_count[i] as f32)
                    .mul_add(left_area[i], right_count[i] as f32 * right_area[i]);
                if cost < best_cost {
                    best_cost = cost;
                    best_axis = axis;
                    best_split = i;
                }
            }
        }

        let node_area = {
            let e = nodes[node_idx].aabb_max - nodes[node_idx].aabb_min;
            e.z.mul_add(e.x, e.x.mul_add(e.y, e.y * e.z)) // omitting 2.0 coefficient
        };
        let leaf_cost = prim_count as f32 * node_area;

        // if making it a leaf is cheaper than the best SAH split, terminate here
        if best_cost >= leaf_cost {
            nodes[node_idx].left_first = start as u32;
            nodes[node_idx].prim_count = prim_count as u32;
            return;
        }

        // partitioning primitives, geometries, and attributes arrays in place
        let bounds_min = centroid_bounds.min[best_axis];
        let bounds_max = centroid_bounds.max[best_axis];
        let scale = BINS as f32 / (bounds_max - bounds_min);

        let mut left = start;
        let mut right = end - 1;

        while left <= right {
            let centroid = prim_infos[left].centroid[best_axis];
            let mut bin_idx = ((centroid - bounds_min) * scale) as usize;
            bin_idx = bin_idx.min(BINS - 1);

            if bin_idx <= best_split {
                left += 1;
            } else {
                prim_infos.swap(left, right);
                geometries.swap(left, right);
                attributes.swap(left, right);
                if right == 0 {
                    break;
                } // safe guard against underflow 
                right -= 1;
            }
        }

        let split_idx = left;

        // edge case: floats caused a weird partition leaving one side completely empty
        if split_idx == start || split_idx == end {
            nodes[node_idx].left_first = start as u32;
            nodes[node_idx].prim_count = prim_count as u32;
            return;
        }

        // create child nodes contiguously (left, right)
        let left_child_idx = nodes.len();
        nodes.push(GpuBvhNode {
            aabb_min: glam::Vec3::ZERO,
            left_first: 0,
            aabb_max: glam::Vec3::ZERO,
            prim_count: 0,
        });
        let right_child_idx = nodes.len();
        nodes.push(GpuBvhNode {
            aabb_min: glam::Vec3::ZERO,
            left_first: 0,
            aabb_max: glam::Vec3::ZERO,
            prim_count: 0,
        });

        nodes[node_idx].left_first = left_child_idx as u32;
        nodes[node_idx].prim_count = 0; // 0 signals non-leaf internal node

        Self::update_node_bounds(left_child_idx, nodes, prim_infos, start, split_idx);
        Self::update_node_bounds(right_child_idx, nodes, prim_infos, split_idx, end);

        Self::subdivide(
            left_child_idx,
            nodes,
            prim_infos,
            geometries,
            attributes,
            start,
            split_idx,
        );
        Self::subdivide(
            right_child_idx,
            nodes,
            prim_infos,
            geometries,
            attributes,
            split_idx,
            end,
        );
    }

    pub fn resize_camera_aspect_ratio(&mut self, width: f32, height: f32) {
        self.camera.aspect_ratio = width / height;
    }

    pub fn move_camera(
        &mut self,
        pressed_keys: &std::collections::HashSet<winit::keyboard::KeyCode>,
        horizontal_speed: f32,
        vertical_speed: f32,
    ) -> bool {
        let w = pressed_keys.contains(&winit::keyboard::KeyCode::KeyW);
        let s = pressed_keys.contains(&winit::keyboard::KeyCode::KeyS);
        let a = pressed_keys.contains(&winit::keyboard::KeyCode::KeyA);
        let d = pressed_keys.contains(&winit::keyboard::KeyCode::KeyD);
        let space = pressed_keys.contains(&winit::keyboard::KeyCode::Space);
        let shift = pressed_keys.contains(&winit::keyboard::KeyCode::ShiftLeft);

        if !(w || s || a || d || space || shift) {
            return false;
        }

        let (sin_yaw, cos_yaw) = self.camera.yaw.sin_cos();

        let forward_xz = glam::Vec3::new(-sin_yaw, 0.0, -cos_yaw);
        let right_xz = glam::Vec3::new(cos_yaw, 0.0, -sin_yaw);

        let forward_coeff = (if w { 1.0 } else { 0.0 }) - (if s { 1.0 } else { 0.0 });
        let right_coeff = (if d { 1.0 } else { 0.0 }) - (if a { 1.0 } else { 0.0 });

        let intent = forward_xz * forward_coeff + right_xz * right_coeff;

        let move_xz = if intent.length_squared() > f32::EPSILON {
            intent.normalize() * horizontal_speed * self.camera.movement_speeds[0]
        } else {
            glam::Vec3::ZERO
        };

        let vert_dir = (if space { 1.0 } else { 0.0 }) - (if shift { 1.0 } else { 0.0 });

        self.camera.position += move_xz
            + glam::Vec3::new(
                0.0,
                vert_dir * vertical_speed * self.camera.movement_speeds[1],
                0.0,
            );

        true
    }

    pub fn rotate_camera(
        &mut self,
        dx: f32,
        dy: f32,
        horizontal_sensitivity: f32,
        vertical_sensitivity: f32,
    ) {
        self.camera.yaw -= dx * horizontal_sensitivity;
        self.camera.pitch -= dy * vertical_sensitivity;

        self.camera.pitch = self
            .camera
            .pitch
            .clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
    }

    pub fn prepare_gpu_camera(&self) -> GpuCamera {
        let cam = &self.camera;

        let rotation_mat =
            glam::Mat3::from_rotation_y(cam.yaw) * glam::Mat3::from_rotation_x(cam.pitch);

        let image_plane_height = 2.0 * (cam.fov_y.to_radians() / 2.0).tan();

        let horizontal3 = rotation_mat.mul_vec3(glam::Vec3::X).normalize()
            * cam.aspect_ratio
            * image_plane_height;

        let vertical3 = rotation_mat.mul_vec3(glam::Vec3::NEG_Y).normalize() * image_plane_height;

        let forward3 = rotation_mat.mul_vec3(glam::Vec3::NEG_Z).normalize();

        let lower_left_corner3 = cam.position + forward3 - horizontal3 / 2.0 - vertical3 / 2.0;

        GpuCamera {
            position: glam::Vec4::from((cam.position, 0.0)),
            lower_left_corner: glam::Vec4::from((lower_left_corner3, 0.0)),
            horizontal: glam::Vec4::from((horizontal3, 0.0)),
            vertical: glam::Vec4::from((vertical3, 0.0)),
        }
    }

    #[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
    fn save_scene_data_json(&self) {
        #[derive(serde::Serialize)]
        struct IndexedGeometry {
            index: usize,
            #[serde(flatten)]
            geometry: GpuTriangleGeometry,
        }

        #[derive(serde::Serialize)]
        struct IndexedAttribute {
            index: usize,
            #[serde(flatten)]
            attribute: GpuTriangleAttribute,
        }

        #[derive(serde::Serialize)]
        struct IndexedMaterial {
            index: usize,
            #[serde(flatten)]
            material: GpuMaterial,
        }

        #[derive(serde::Serialize)]
        struct SceneData {
            geometries: Vec<IndexedGeometry>,
            attributes: Vec<IndexedAttribute>,
            materials: Vec<IndexedMaterial>,
        }

        let scene_data = SceneData {
            geometries: self
                .geometries
                .iter()
                .enumerate()
                .map(|(index, geometry)| IndexedGeometry {
                    index,
                    geometry: *geometry,
                })
                .collect(),
            attributes: self
                .attributes
                .iter()
                .enumerate()
                .map(|(index, attribute)| IndexedAttribute {
                    index,
                    attribute: *attribute,
                })
                .collect(),
            materials: self
                .materials
                .iter()
                .enumerate()
                .map(|(index, material)| IndexedMaterial {
                    index,
                    material: *material,
                })
                .collect(),
        };

        let mut output_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        output_path.push("debug_output");

        if let Err(e) = std::fs::create_dir_all(&output_path) {
            log::error!("Failed to create debug_output directory: {e}");
            return;
        }

        output_path.push("scene_data.yaml");

        match serde_yaml::to_string(&scene_data) {
            Ok(yaml) => match std::fs::write(&output_path, yaml) {
                Ok(()) => {
                    log::info!(
                        "Saved scene data YAML to {} (geometries: {}, attributes: {}, materials: {})",
                        output_path.display(),
                        self.geometries.len(),
                        self.attributes.len(),
                        self.materials.len()
                    );
                }
                Err(e) => {
                    log::error!("Failed to write scene data YAML: {e}");
                }
            },
            Err(e) => {
                log::error!("Failed to serialize scene data to YAML: {e}");
            }
        }
    }

    #[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
    fn save_rgba_image(filename: &str, pixels: &[u8], width: u32, height: u32) {
        // create debug_output directory in the crate root
        let mut output_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        output_path.push("debug_output");

        if let Err(e) = std::fs::create_dir_all(&output_path) {
            log::error!("Failed to create debug_output directory: {e}");
            return;
        }

        output_path.push(filename);

        match image::RgbaImage::from_raw(width, height, pixels.to_vec()) {
            Some(img) => match img.save(&output_path) {
                Ok(()) => {
                    log::info!("Saved debug image to {}", output_path.display());
                }
                Err(e) => {
                    log::error!("Failed to save image {}: {}", output_path.display(), e);
                }
            },
            None => {
                log::error!(
                    "Failed to create image from raw pixels: {}x{} with {} bytes",
                    width,
                    height,
                    pixels.len()
                );
            }
        }
    }
}
