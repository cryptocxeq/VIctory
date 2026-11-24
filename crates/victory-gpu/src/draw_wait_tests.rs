//! Render a resumed segment and verify its fixed-function state on D3D12.

use super::*;
use windows::Win32::Graphics::Direct3D::{Fxc::D3DCompile, ID3DBlob};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32G32B32A32_FLOAT;
use windows::core::{PCSTR, s};

fn shader(entry: PCSTR, model: PCSTR) -> ID3DBlob {
    const SOURCE: &str = r#"
        [RootSignature("")]
        float4 vs(uint id : SV_VertexID) : SV_Position {
            float2 p = float2((id << 1) & 2, id & 2);
            return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
        }
        float4 ps() : SV_Target { return float4(1, 1, 1, 1); }
    "#;
    let mut code = None;
    // SAFETY: source and entry names live through compilation; output is ours.
    unsafe {
        D3DCompile(
            SOURCE.as_ptr().cast(),
            SOURCE.len(),
            s!("wait-state"),
            None,
            None,
            entry,
            model,
            0,
            0,
            &mut code,
            None,
        )
        .unwrap();
    }
    code.unwrap()
}

#[test]
fn memory_wait_resume_restores_render_state() {
    let host = crate::host::host().expect("render-state regression requires D3D12");
    let device = &host.device;
    let (vs, ps) = (
        shader(s!("vs"), s!("vs_5_1")),
        shader(s!("ps"), s!("ps_5_1")),
    );
    // SAFETY: all descriptions and shader bytes outlive their calls, every
    // resource stays alive through synchronous host execution, and mapped
    // readback memory is read only after that execution completes.
    unsafe {
        let bytes = |blob: &ID3DBlob| {
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
        };
        let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes(&vs)).unwrap();
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vs.GetBufferPointer(),
                BytecodeLength: vs.GetBufferSize(),
            },
            PS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: ps.GetBufferPointer(),
                BytecodeLength: ps.GetBufferSize(),
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        desc.RTVFormats[0] = DXGI_FORMAT_R32G32B32A32_FLOAT;
        desc.BlendState.RenderTarget[0] = D3D12_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: D3D12_BLEND_BLEND_FACTOR,
            DestBlend: D3D12_BLEND_ZERO,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_ZERO,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
            ..Default::default()
        };
        let pso: ID3D12PipelineState = device.CreateGraphicsPipelineState(&desc).unwrap();
        let texture_desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 2,
            Height: 2,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
            SampleDesc: desc.SampleDesc,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            ..Default::default()
        };
        let make_target = || {
            let mut target: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_DEFAULT,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &texture_desc,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    None,
                    &mut target,
                )
                .unwrap();
            target.unwrap()
        };
        let target = make_target();
        let before = host.new_list().unwrap();
        let mut state = State::default();
        state.set_topology(&before, 4);
        state.set_viewports(
            &before,
            &[D3D12_VIEWPORT {
                Width: 1.0,
                Height: 1.0,
                MaxDepth: 1.0,
                ..Default::default()
            }],
        );
        state.set_scissors(
            &before,
            &[RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            }],
        );
        state.set_blend_factor(&before, Some(&[0.25, 0.5, 0.75, 1.0]));
        state.target_views.push((target.clone(), None));
        let rtv = host.rtv(&target, None);
        before.list.OMSetRenderTargets(1, Some(&rtv), false, None);
        before.list.ClearRenderTargetView(rtv, &[0.0; 4], None);
        before.close();
        host.execute(&[&before]);
        let after = host.new_list().unwrap();
        state.resume(&after);
        after.list.SetGraphicsRootSignature(&root);
        after.list.SetPipelineState(&pso);
        after.list.DrawInstanced(3, 1, 0, 0);
        after.list.ResourceBarrier(&[crate::host::transition(
            &target,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        )]);
        let mut readback: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_READBACK,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: 512,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: desc.SampleDesc,
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )
            .unwrap();
        let readback = readback.unwrap();
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&readback),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                        Width: 2,
                        Height: 2,
                        Depth: 1,
                        RowPitch: 256,
                    },
                },
            },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&target),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        after.list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
        after.close();
        host.execute(&[&after]);
        let mut mapped = std::ptr::null_mut();
        readback
            .Map(
                0,
                Some(&D3D12_RANGE { Begin: 0, End: 512 }),
                Some(&mut mapped),
            )
            .unwrap();
        let pixel = |offset: usize| {
            mapped
                .cast::<u8>()
                .add(offset)
                .cast::<[f32; 4]>()
                .read_unaligned()
        };
        let values = [pixel(0), pixel(16), pixel(256), pixel(272)];
        readback.Unmap(0, Some(&D3D12_RANGE::default()));
        assert_eq!(
            values,
            [[0.25, 0.5, 0.75, 1.0], [0.0; 4], [0.0; 4], [0.0; 4]]
        );
    }
}
