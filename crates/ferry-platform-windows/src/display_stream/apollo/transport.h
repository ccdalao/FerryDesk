#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct sf_apollo_media sf_apollo_media;
typedef struct sf_apollo_media_stats {
  uint64_t sent, received, invalid, recovered, lost, frames, qos;
} sf_apollo_media_stats;

// A small C ABI around the vendored Apollo/Moonlight media primitives. The
// socket is deliberately owned by the platform worker so Rust never owns a
// WinSock handle or crosses a callback boundary.
sf_apollo_media *sf_apollo_media_create(uint16_t local_port, uint16_t mtu, uint8_t fec_percent);
uint16_t sf_apollo_media_local_port(const sf_apollo_media *media);
int sf_apollo_media_set_peer(sf_apollo_media *media, const char *ipv4, uint16_t port, uint64_t token);
int sf_apollo_media_set_key(sf_apollo_media *media, const uint8_t *key);
int sf_apollo_media_probe(sf_apollo_media *media, int source, uint32_t wait_ms);
void sf_apollo_media_pacing(sf_apollo_media *media, uint32_t bitrate);
void sf_apollo_media_stats_read(sf_apollo_media *media, sf_apollo_media_stats *stats);
int sf_apollo_media_send_frame(sf_apollo_media *media, const uint8_t *data, uint32_t length,
    uint32_t frame_index, uint64_t timestamp_us, uint8_t track);
int sf_apollo_media_receive_frame(sf_apollo_media *media, uint8_t *out, uint32_t capacity,
    uint32_t *length, uint32_t *frame_index, uint64_t *timestamp_us, uint8_t *track,
    uint32_t wait_ms);
void sf_apollo_media_destroy(sf_apollo_media *media);
const char *sf_apollo_media_error(void);

#ifdef __cplusplus
}
#endif
