// ScreenFerry C ABI for Apollo v0.4.6's original native D3D11 NVENC implementation.
// SPDX-License-Identifier: GPL-3.0-only
#include "src/logging.h"
#include "src/nvenc/nvenc_d3d11_native.h"

namespace {
  struct state {
    nvenc::nvenc_d3d11_native encoder;
    nvenc::nvenc_encoded_frame frame;
    uint64_t next = 0;
    bool idr = true;
    explicit state(ID3D11Device *device): encoder(device) {}
  };
}

extern "C" void *sf_apollo_nvenc_create(ID3D11Device *device, uint32_t width, uint32_t height,
    uint32_t refresh, uint32_t bitrate, screenferry::log_callback callback) noexcept {
  screenferry::callback = callback;
  screenferry::last_error.clear();
  if (!device || width != 1920 || height != 1080 || refresh < 60 || refresh > 100) return nullptr;
  try {
    auto encoder = std::make_unique<state>(device);
    video::config_t stream{};
    stream.width = width; stream.height = height; stream.framerate = refresh;
    stream.encodingFramerate = refresh; stream.bitrate = bitrate / 1000;
    stream.slicesPerFrame = 1; stream.numRefFrames = 1;
    const nvenc::nvenc_config config{};
    const nvenc::nvenc_colorspace_t colorspace {
      NV_ENC_VUI_COLOR_PRIMARIES_SMPTE170M, NV_ENC_VUI_TRANSFER_CHARACTERISTIC_SMPTE170M,
      NV_ENC_VUI_MATRIX_COEFFS_SMPTE170M, false
    };
    if (!encoder->encoder.create_encoder(config, stream, colorspace, NV_ENC_BUFFER_FORMAT_NV12)) return nullptr;
    return encoder.release();
  } catch (...) { screenferry::last_error = "Apollo NVENC initialization raised an exception"; return nullptr; }
}

extern "C" ID3D11Texture2D *sf_apollo_nvenc_texture(void *handle) noexcept {
  if (!handle) return nullptr;
  auto texture = static_cast<state *>(handle)->encoder.get_input_texture();
  if (texture) texture->AddRef(); // Caller owns this one reference.
  return texture;
}

extern "C" int sf_apollo_nvenc_encode(void *handle, const uint8_t **data, size_t *size) noexcept {
  if (!handle || !data || !size) return 0;
  try {
    auto &encoder = *static_cast<state *>(handle);
    const auto index = encoder.next++;
    encoder.frame = encoder.encoder.encode_frame(index, encoder.idr);
    encoder.idr = false;
    if (encoder.frame.data.empty()) return 0;
    *data = encoder.frame.data.data(); *size = encoder.frame.data.size();
    return 1; // Data stays valid until the next encode or destroy on this handle.
  } catch (...) { screenferry::last_error = "Apollo NVENC encoding raised an exception"; return 0; }
}

extern "C" const char *sf_apollo_nvenc_error() noexcept { return screenferry::last_error.c_str(); }
extern "C" void sf_apollo_nvenc_request_idr(void *handle) noexcept {
  if (handle) static_cast<state *>(handle)->idr = true;
}
extern "C" int sf_apollo_nvenc_bitrate(void *handle, uint32_t bitrate) noexcept {
  if (!handle) return 0;
  try { return static_cast<state *>(handle)->encoder.set_bitrate(bitrate) ? 1 : 0; }
  catch (...) { return 0; }
}
extern "C" void sf_apollo_nvenc_destroy(void *handle) noexcept { delete static_cast<state *>(handle); }
