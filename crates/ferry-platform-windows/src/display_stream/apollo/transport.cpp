// Apollo v0.4.6 RTP/FEC sender, adapted to ScreenFerry-owned sockets and CNG.
// Receiver reordering and FEC recovery execute the pinned Moonlight RtpVideoQueue.c.
// SPDX-License-Identifier: GPL-3.0-only
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <bcrypt.h>
#include <qos2.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cstring>
#include <deque>
#include <memory>
#include <mutex>
#include <new>
#include <string>
#include <vector>
extern "C" {
#include "moonlight_shim.h"
#include "upstream/moonlight-common-c/reedsolomon/rs.h"
}
#include "transport.h"

using clock_type = std::chrono::steady_clock;
constexpr size_t MAX_FRAME = 2 * 1024 * 1024;
constexpr size_t RTP_HEADER = 32;
constexpr size_t MAX_SHARDS = 255;
constexpr size_t MAX_BLOCKS = 4;
thread_local std::string last_error;
// Apollo loads qWAVE dynamically because some Windows installations omit it.
struct qwave_api {
  decltype(&QOSCreateHandle) create = nullptr;
  decltype(&QOSAddSocketToFlow) add = nullptr;
  decltype(&QOSRemoveSocketFromFlow) remove = nullptr;
  decltype(&QOSCloseHandle) close = nullptr;
  qwave_api() {
    auto module = LoadLibraryExW(L"qwave.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!module) return;
    create = reinterpret_cast<decltype(create)>(GetProcAddress(module, "QOSCreateHandle"));
    add = reinterpret_cast<decltype(add)>(GetProcAddress(module, "QOSAddSocketToFlow"));
    remove = reinterpret_cast<decltype(remove)>(GetProcAddress(module, "QOSRemoveSocketFromFlow"));
    close = reinterpret_cast<decltype(close)>(GetProcAddress(module, "QOSCloseHandle"));
  }
};
static qwave_api &qwave() { static qwave_api api; return api; }
extern "C" {
const int AppVersionQuad[4] = {7, 1, 431, 0};
__declspec(thread) STREAM_CONFIGURATION StreamConfig = {};
uint64_t PltGetMillis() { return GetTickCount64() + 1; }
}

#pragma pack(push, 1)
struct raw_packet { RTP_PACKET rtp; uint32_t extension; NV_VIDEO_PACKET nv; };
struct short_header { uint8_t type; uint16_t latency; uint8_t frame_type; uint16_t last_size; uint16_t reserved; };
#pragma pack(pop)
static_assert(sizeof(raw_packet) == RTP_HEADER);
static_assert(sizeof(short_header) == 8);

struct output_frame { std::vector<uint8_t> bytes; uint32_t index; uint64_t timestamp; uint8_t track; };
struct track_state {
  RTP_VIDEO_QUEUE queue{};
  uint16_t sequence = 0;
  std::vector<uint8_t> assembly;
  uint32_t assembling_frame = 0;
  uint16_t last_payload = 0;
};
struct sf_apollo_media {
  SOCKET socket = INVALID_SOCKET;
  uint16_t local_port = 0;
  size_t block_size = 1168;
  uint8_t fec = 20;
  BCRYPT_ALG_HANDLE algorithm = nullptr;
  BCRYPT_KEY_HANDLE key = nullptr;
  std::vector<uint8_t> key_object;
  uint64_t counter = 0;
  // Separate nonce domains for media and the two probe directions.
  uint64_t highest_counter[4] = {};
  std::array<std::array<uint64_t, 64>, 4> replay{};
  track_state tracks[2];
  std::deque<output_frame> output;
  sf_apollo_media_stats stats{};
  HANDLE timer = nullptr;
  HANDLE qos = nullptr;
  QOS_FLOWID flow = 0;
  clock_type::time_point next_send{};
  uint32_t pacing_bps = 80'000'000;
  ~sf_apollo_media() {
    for (auto &track : tracks) RtpvCleanupQueue(&track.queue);
    if (flow) qwave().remove(qos, socket, flow, 0);
    if (qos) qwave().close(qos);
    if (timer) CloseHandle(timer);
    if (key) BCryptDestroyKey(key);
    if (algorithm) BCryptCloseAlgorithmProvider(algorithm, 0);
    if (socket != INVALID_SOCKET) closesocket(socket);
  }
};
thread_local sf_apollo_media *active_media = nullptr;
thread_local uint8_t active_track = 0;

extern "C" void connectionSawFrame(uint32_t) {}
extern "C" void notifyFrameLost(uint32_t, bool speculative) {
  if (active_media && !speculative) ++active_media->stats.lost;
}
extern "C" void connectionSendFrameFecStatus(PSS_FRAME_FEC_STATUS status) {
  if (active_media) {
    auto data = BE16(status->totalDataPackets);
    auto received = BE16(status->receivedDataPackets);
    if (received < data && received + BE16(status->receivedParityPackets) >= data)
      active_media->stats.recovered += data - received;
  }
}
extern "C" void queueRtpPacket(PRTPV_QUEUE_ENTRY entry) {
  // Moonlight submits only completely recovered, ordered frames here. Ownership
  // of every original/reconstructed allocation transfers to this callback.
  auto &media = *active_media;
  auto &track = media.tracks[active_track];
  auto *raw = reinterpret_cast<raw_packet *>(entry->packet);
  const auto block = (raw->nv.multiFecBlocks >> 4) & 3;
  const auto last_block = (raw->nv.multiFecBlocks >> 6) & 3;
  const auto index = raw->nv.frameIndex;
  auto *payload = reinterpret_cast<uint8_t *>(raw) + RTP_HEADER;
  size_t length = entry->length - RTP_HEADER;
  if (block == 0 && (raw->nv.flags & FLAG_SOF)) {
    track.assembly.clear();
    track.assembling_frame = index;
    short_header header{};
    if (length >= sizeof(header)) std::memcpy(&header, payload, sizeof(header));
    track.last_payload = header.type == 1 ? header.last_size : 0;
  }
  const bool end = block == last_block && (raw->nv.flags & FLAG_EOF);
  if (end) length = std::min<size_t>(length, track.last_payload);
  if (track.assembling_frame == index && track.last_payload && length
      && track.assembly.size() + length <= MAX_FRAME + sizeof(short_header)) {
    track.assembly.insert(track.assembly.end(), payload, payload + length);
    if (end && track.assembly.size() > sizeof(short_header)) {
      output_frame frame;
      frame.bytes.assign(track.assembly.begin() + sizeof(short_header), track.assembly.end());
      frame.index = index;
      frame.timestamp = static_cast<uint64_t>(raw->rtp.timestamp) * 100 / 9;
      frame.track = active_track;
      // Overflow must be reported so the caller requests IDR before decoding
      // any dependent frame. Never silently discard a compressed P-frame.
      if (media.output.size() >= 4) { media.output.clear(); ++media.stats.lost; }
      media.output.push_back(std::move(frame));
      ++media.stats.frames;
      track.assembly.clear();
    }
  }
  free(entry->packet);
}

static void fail(const char *message) { last_error = message; }
static bool crypto(sf_apollo_media &media, bool encrypt, ENC_VIDEO_HEADER &prefix,
    uint8_t *bytes, size_t length) {
  BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO auth;
  BCRYPT_INIT_AUTH_MODE_INFO(auth);
  auth.pbNonce = prefix.iv; auth.cbNonce = sizeof(prefix.iv);
  auth.pbTag = prefix.tag; auth.cbTag = sizeof(prefix.tag);
  auth.pbAuthData = reinterpret_cast<PUCHAR>(&prefix.frameNumber);
  auth.cbAuthData = sizeof(prefix.frameNumber);
  ULONG result = 0;
  auto status = encrypt
    ? BCryptEncrypt(media.key, bytes, static_cast<ULONG>(length), &auth, nullptr, 0,
        bytes, static_cast<ULONG>(length), &result, 0)
    : BCryptDecrypt(media.key, bytes, static_cast<ULONG>(length), &auth, nullptr, 0,
        bytes, static_cast<ULONG>(length), &result, 0);
  return status >= 0 && result == length;
}
static bool send_packet(sf_apollo_media &media, const uint8_t *data, size_t length,
    uint32_t frame, uint8_t domain) {
  if (!media.key || media.counter == UINT64_MAX) return false;
  std::array<uint8_t, 1500> buffer{};
  if (length + sizeof(ENC_VIDEO_HEADER) > buffer.size()) return false;
  ENC_VIDEO_HEADER prefix{};
  auto counter = ++media.counter;
  std::memcpy(prefix.iv, &counter, sizeof(counter));
  prefix.iv[11] = domain;
  prefix.frameNumber = frame;
  auto *payload = buffer.data() + sizeof(prefix);
  std::memcpy(payload, data, length);
  if (!crypto(media, true, prefix, payload, length)) return false;
  std::memcpy(buffer.data(), &prefix, sizeof(prefix));
  int sent = send(media.socket, reinterpret_cast<const char *>(buffer.data()),
      static_cast<int>(sizeof(prefix) + length), 0);
  if (sent != sizeof(prefix) + length) return false;
  ++media.stats.sent;
  return true;
}
static int receive_packet(sf_apollo_media &media, std::vector<uint8_t> &plain,
    ENC_VIDEO_HEADER &prefix, uint32_t wait_ms) {
  fd_set readset; FD_ZERO(&readset); FD_SET(media.socket, &readset);
  timeval wait{static_cast<long>(wait_ms / 1000), static_cast<long>(wait_ms % 1000 * 1000)};
  int result = select(0, &readset, nullptr, nullptr, &wait);
  if (result <= 0) return result;
  std::array<uint8_t, 1500> packet;
  int length = recv(media.socket, reinterpret_cast<char *>(packet.data()), static_cast<int>(packet.size()), 0);
  if (length == SOCKET_ERROR) {
    return WSAGetLastError() == WSAECONNRESET ? 0 : -1;
  }
  if (length <= static_cast<int>(sizeof(prefix))) return 0;
  std::memcpy(&prefix, packet.data(), sizeof(prefix));
  const uint8_t domain = prefix.iv[11];
  if (domain > 3 || !media.key) return 0;
  plain.assign(packet.begin() + sizeof(prefix), packet.begin() + length);
  if (!crypto(media, false, prefix, plain.data(), plain.size())) { ++media.stats.invalid; return 0; }
  uint64_t counter; std::memcpy(&counter, prefix.iv, sizeof(counter));
  auto &high = media.highest_counter[domain];
  // 4096-packet replay window permits normal cross-batch reordering.
  if (!counter || (counter <= high && (high - counter >= 4096
      || counter / 64 + 64 <= high / 64))) return 0;
  auto &slot = media.replay[domain][(counter / 64) % 64];
  if (counter > high) {
    for (uint64_t group = high / 64 + 1; group <= counter / 64 && group < high / 64 + 66; ++group)
      media.replay[domain][group % 64] = 0;
    high = counter;
  }
  const auto mask = uint64_t(1) << (counter % 64);
  if (slot & mask) return 0;
  slot |= mask;
  ++media.stats.received;
  return 1;
}
static void pace(sf_apollo_media &media, size_t bytes) {
  auto now = clock_type::now();
  media.next_send = std::max(media.next_send, now);
  auto delay = media.next_send - now;
  if (delay > std::chrono::microseconds(50)) {
    LARGE_INTEGER due; due.QuadPart = -std::max<int64_t>(1,
      std::chrono::duration_cast<std::chrono::nanoseconds>(delay).count() / 100);
    if (SetWaitableTimer(media.timer, &due, 0, nullptr, nullptr, FALSE))
      WaitForSingleObject(media.timer, 100);
  }
  media.next_send += std::chrono::nanoseconds(bytes * 8'000'000'000ULL / media.pacing_bps);
}

extern "C" sf_apollo_media *sf_apollo_media_create(uint16_t port, uint16_t mtu, uint8_t fec) {
  try {
    static std::once_flag init;
    static bool okay = false;
    std::call_once(init, [] { WSADATA data{}; okay = WSAStartup(MAKEWORD(2,2), &data) == 0; reed_solomon_init(); });
    if (!okay) return nullptr;
    auto media = std::make_unique<sf_apollo_media>();
    media->block_size = (std::clamp<size_t>(mtu, 576, 1400) - sizeof(ENC_VIDEO_HEADER)) / 16 * 16;
    media->fec = std::clamp<uint8_t>(fec, 1, 40);
    media->socket = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    if (media->socket == INVALID_SOCKET) return nullptr;
    int size = 1024 * 1024;
    setsockopt(media->socket, SOL_SOCKET, SO_RCVBUF, reinterpret_cast<char *>(&size), sizeof(size));
    // A bounded OS send queue avoids seconds of invisible stale video.
    size = 64 * 1024;
    setsockopt(media->socket, SOL_SOCKET, SO_SNDBUF, reinterpret_cast<char *>(&size), sizeof(size));
    DWORD send_timeout = 100;
    setsockopt(media->socket, SOL_SOCKET, SO_SNDTIMEO, reinterpret_cast<char *>(&send_timeout), sizeof(send_timeout));
    sockaddr_in address{}; address.sin_family = AF_INET; address.sin_port = htons(port);
    if (bind(media->socket, reinterpret_cast<sockaddr *>(&address), sizeof(address))) return nullptr;
    int addr_size = sizeof(address);
    if (getsockname(media->socket, reinterpret_cast<sockaddr *>(&address), &addr_size)) return nullptr;
    media->local_port = ntohs(address.sin_port);
    media->timer = CreateWaitableTimerExW(nullptr, nullptr, 2, TIMER_ALL_ACCESS);
    if (!media->timer) media->timer = CreateWaitableTimerW(nullptr, FALSE, nullptr);
    if (!media->timer) return nullptr;
    for (auto &track : media->tracks) RtpvInitializeQueue(&track.queue);
    return media.release();
  } catch (...) { fail("native media allocation failed"); return nullptr; }
}
extern "C" uint16_t sf_apollo_media_local_port(const sf_apollo_media *media) { return media->local_port; }
extern "C" int sf_apollo_media_set_peer(sf_apollo_media *media, const char *ip, uint16_t port, uint64_t) {
  sockaddr_in address{}; address.sin_family = AF_INET; address.sin_port = htons(port);
  if (!media || !port || inet_pton(AF_INET, ip, &address.sin_addr) != 1
      || connect(media->socket, reinterpret_cast<sockaddr *>(&address), sizeof(address))) return 0;
  QOS_VERSION version{1,0};
  auto &api = qwave();
  if (api.create && api.add && api.remove && api.close && api.create(&version, &media->qos)) {
    media->stats.qos = api.add(media->qos, media->socket,
      // TCP input/control uses Control priority; media remains below it.
      reinterpret_cast<sockaddr *>(&address), QOSTrafficTypeAudioVideo, QOS_NON_ADAPTIVE_FLOW, &media->flow) != FALSE;
  }
  return 1;
}
extern "C" int sf_apollo_media_set_key(sf_apollo_media *media, const uint8_t *key) {
  if (!media || !key || media->key) return 0;
  if (BCryptOpenAlgorithmProvider(&media->algorithm, BCRYPT_AES_ALGORITHM, nullptr, 0) < 0) return 0;
  if (BCryptSetProperty(media->algorithm, BCRYPT_CHAINING_MODE,
      reinterpret_cast<PUCHAR>(const_cast<wchar_t *>(BCRYPT_CHAIN_MODE_GCM)), sizeof(BCRYPT_CHAIN_MODE_GCM), 0) < 0) return 0;
  ULONG size = 0, used = 0;
  if (BCryptGetProperty(media->algorithm, BCRYPT_OBJECT_LENGTH, reinterpret_cast<PUCHAR>(&size), sizeof(size), &used, 0) < 0) return 0;
  media->key_object.resize(size);
  return BCryptGenerateSymmetricKey(media->algorithm, &media->key, media->key_object.data(), size,
      const_cast<PUCHAR>(key), 32, 0) >= 0;
}
extern "C" int sf_apollo_media_probe(sf_apollo_media *media, int source, uint32_t wait_ms) {
  if (!media || !media->key) return 0;
  auto deadline = clock_type::now() + std::chrono::milliseconds(wait_ms);
  auto next = clock_type::now();
  uint8_t domain = source ? 2 : 3;
  bool heard = false;
  while (clock_type::now() < deadline) {
    if (clock_type::now() >= next) {
      uint32_t probe = heard ? 0x504f4e47 : 0x50494e47;
      if (!send_packet(*media, reinterpret_cast<uint8_t *>(&probe), sizeof(probe), 0, domain)) return 0;
      next = clock_type::now() + std::chrono::milliseconds(20);
    }
    ENC_VIDEO_HEADER prefix{}; std::vector<uint8_t> bytes;
    if (receive_packet(*media, bytes, prefix, 10) == 1 && bytes.size() == 4
        && prefix.iv[11] == (source ? 3 : 2)) {
      uint32_t probe; std::memcpy(&probe, bytes.data(), 4);
      if (probe == 0x504f4e47 || probe == 0x50494e47) heard = true;
    }
  }
  return heard ? 1 : 0;
}
extern "C" void sf_apollo_media_pacing(sf_apollo_media *media, uint32_t bitrate) {
  media->pacing_bps = std::clamp<uint32_t>(bitrate, 8'000'000, 200'000'000);
}
extern "C" void sf_apollo_media_stats_read(sf_apollo_media *media, sf_apollo_media_stats *stats) { *stats = media->stats; }

extern "C" int sf_apollo_media_send_frame(sf_apollo_media *media, const uint8_t *bytes,
    uint32_t length, uint32_t frame_index, uint64_t timestamp_us, uint8_t track_id) {
  try {
    if (!media || !media->key || !length || length > MAX_FRAME || track_id > 1) return 0;
    auto &track = media->tracks[track_id];
    const size_t block_size = media->block_size, payload_size = block_size - RTP_HEADER;
    short_header header{1, 0, 1, static_cast<uint16_t>((length + 8) % payload_size), 0};
    if (track_id == 0) {
      for (size_t offset = 0; offset + 3 < length; ++offset) {
        if (!bytes[offset] && !bytes[offset+1] && bytes[offset+2] == 1
            && (bytes[offset+3] & 31) == 5) { header.frame_type = 2; break; }
      }
    }
    if (!header.last_size) header.last_size = static_cast<uint16_t>(payload_size);
    std::vector<uint8_t> payload(length + 8);
    std::memcpy(payload.data(), &header, 8); std::memcpy(payload.data()+8, bytes, length);
    size_t packets = (payload.size() + payload_size - 1) / payload_size;
    size_t fec = media->fec;
    size_t max_data = MAX_SHARDS * 100 / (100 + fec);
    size_t blocks = (packets + max_data - 1) / max_data;
    if (blocks > MAX_BLOCKS) { fec = 0; blocks = MAX_BLOCKS; }
    const size_t per_block = (packets + blocks - 1) / blocks;
    if (per_block > 1023) { fail("encoded frame exceeds Apollo RTP packet bound"); return 0; }
    // This is Apollo's stream.cpp packet format/FEC construction. RS is over
    // the complete RTP+NV shard; mutable RTP/FEC fields are finalized afterward.
    for (size_t block = 0; block < blocks; ++block) {
      const size_t count = std::min(per_block, packets - block * per_block);
      size_t parity = fec ? (count * fec + 99) / 100 : 0;
      const size_t total = count + parity;
      std::vector<std::vector<uint8_t>> shards(total, std::vector<uint8_t>(block_size));
      std::vector<unsigned char *> pointers(total);
      for (size_t i = 0; i < total; ++i) pointers[i] = shards[i].data();
      for (size_t i = 0; i < count; ++i) {
        auto *raw = reinterpret_cast<raw_packet *>(pointers[i]);
        raw->nv.frameIndex = frame_index;
        raw->nv.streamPacketIndex = (uint32_t(track.sequence) + uint32_t(i)) << 8;
        raw->nv.multiFecFlags = 0x10;
        raw->nv.multiFecBlocks = static_cast<uint8_t>((block << 4) | ((blocks-1) << 6));
        raw->nv.flags = FLAG_CONTAINS_PIC_DATA | (i==0 ? FLAG_SOF : 0) | (i+1==count ? FLAG_EOF : 0);
        size_t offset = (block * per_block + i) * payload_size;
        std::memcpy(pointers[i]+RTP_HEADER, payload.data()+offset, std::min(payload_size, payload.size()-offset));
      }
      if (parity) {
        auto *rs = reed_solomon_new(static_cast<int>(count), static_cast<int>(parity));
        if (!rs) return 0;
        int result = reed_solomon_encode(rs, pointers.data(), static_cast<int>(total), static_cast<int>(block_size));
        reed_solomon_release(rs);
        if (result) return 0;
      }
      // Apollo batches at most 64 packets / 64KiB. Bound bursts to ~1ms of
      // the current hotspot budget instead of assuming 800Mbps capacity.
      const size_t batch = std::max<size_t>(1, std::min<size_t>({64, 65536/(block_size+32), media->pacing_bps/8000/(block_size+32)}));
      for (size_t i = 0; i < total; ++i) {
        auto *raw = reinterpret_cast<raw_packet *>(pointers[i]);
        raw->nv.fecInfo = static_cast<uint32_t>((i<<12) | (count<<22) | (fec<<4));
        raw->nv.multiFecBlocks = static_cast<uint8_t>((block<<4) | ((blocks-1)<<6));
        raw->nv.frameIndex = frame_index;
        raw->rtp.header = 0x90; raw->rtp.sequenceNumber = htons(track.sequence + static_cast<uint16_t>(i));
        raw->rtp.timestamp = htonl(static_cast<uint32_t>(timestamp_us * 9 / 100));
        if (i % batch == 0) pace(*media, std::min(batch, total-i)*(block_size+32));
        if (!send_packet(*media, pointers[i], block_size, frame_index, track_id)) { fail("encrypted UDP send failed"); return 0; }
      }
      track.sequence += static_cast<uint16_t>(total);
    }
    return 1;
  } catch (...) { fail("Apollo packetization allocation failed"); return 0; }
}

extern "C" int sf_apollo_media_receive_frame(sf_apollo_media *media, uint8_t *out, uint32_t capacity,
    uint32_t *length, uint32_t *index, uint64_t *timestamp, uint8_t *track_id, uint32_t wait_ms) {
  try {
    if (!media || !media->key) return -1;
    active_media = media;
    StreamConfig.packetSize = static_cast<int>(media->block_size - MAX_RTP_HEADER_SIZE);
    auto deadline = clock_type::now() + std::chrono::milliseconds(wait_ms);
    do {
      if (!media->output.empty()) {
        auto frame = std::move(media->output.front()); media->output.pop_front();
        if (frame.bytes.size() > capacity) return -1;
        std::memcpy(out, frame.bytes.data(), frame.bytes.size());
        *length = static_cast<uint32_t>(frame.bytes.size()); *index=frame.index; *timestamp=frame.timestamp; *track_id=frame.track;
        return 1;
      }
      ENC_VIDEO_HEADER prefix{}; std::vector<uint8_t> bytes;
      int result = receive_packet(*media, bytes, prefix, 1);
      if (result < 0) { fail("UDP receive failed"); return -1; }
      if (result == 0 || prefix.iv[11] > 1 || bytes.size() != media->block_size) continue;
      auto *raw = reinterpret_cast<raw_packet *>(bytes.data());
      const uint32_t fec = (raw->nv.fecInfo >> 4) & 255;
      const uint32_t data = raw->nv.fecInfo >> 22;
      const uint32_t shard = (raw->nv.fecInfo >> 12) & 1023;
      const uint32_t total = data + (data*fec+99)/100;
      if (raw->rtp.header != 0x90 || raw->nv.frameIndex != prefix.frameNumber
          || !data || fec>40 || (fec && total>255) || shard>=total
          || ((raw->nv.multiFecBlocks>>4)&3)>((raw->nv.multiFecBlocks>>6)&3)) { ++media->stats.invalid; continue; }
      active_track = prefix.iv[11];
      auto &queue = media->tracks[active_track].queue;
      if (queue.pendingFecBlockList.count && queue.currentFrameNumber == raw->nv.frameIndex
          && queue.multiFecCurrentBlockNumber == ((raw->nv.multiFecBlocks>>4)&3)
          && (queue.bufferDataPackets != data || queue.fecPercentage != fec
              || U16(ntohs(raw->rtp.sequenceNumber)-shard) != queue.bufferLowestSequenceNumber)) { ++media->stats.invalid; continue; }
      auto *packet = static_cast<uint8_t *>(calloc(1, media->block_size + sizeof(RTPV_QUEUE_ENTRY)));
      if (!packet) return -1;
      std::memcpy(packet, bytes.data(), bytes.size());
      auto *rtp = reinterpret_cast<PRTP_PACKET>(packet);
      rtp->sequenceNumber = ntohs(rtp->sequenceNumber); rtp->timestamp = ntohl(rtp->timestamp);
      auto *entry = reinterpret_cast<PRTPV_QUEUE_ENTRY>(packet + media->block_size);
      if (RtpvAddPacket(&queue, rtp, static_cast<int>(media->block_size), entry) == RTPF_RET_REJECTED) free(packet);
    } while (clock_type::now() < deadline);
    return 0;
  } catch (...) { fail("Moonlight queue allocation failed"); return -1; }
}
extern "C" void sf_apollo_media_destroy(sf_apollo_media *media) { delete media; }
extern "C" const char *sf_apollo_media_error() { return last_error.c_str(); }

// TCP input/control flows own a duplicated connected socket in the Rust guard.
struct sf_tcp_priority {
  SOCKET socket; HANDLE qos = nullptr; QOS_FLOWID flow = 0; unsigned traffic = 0;
  ~sf_tcp_priority() { if (flow) qwave().remove(qos, socket, flow, 0); if(qos) qwave().close(qos); }
};
extern "C" int sf_network_priority_set(void *handle, unsigned traffic) {
  auto *p = static_cast<sf_tcp_priority *>(handle);
  if (!p || traffic > 5) return 0;
  if (p->flow && p->traffic == traffic) return 1;
  if (p->flow) { if (!qwave().remove(p->qos,p->socket,p->flow,0)) return 0; p->flow=0; }
  if (!qwave().add(p->qos,p->socket,nullptr,static_cast<QOS_TRAFFIC_TYPE>(traffic),QOS_NON_ADAPTIVE_FLOW,&p->flow)) return 0;
  p->traffic=traffic;return 1;
}
extern "C" void *sf_network_priority_new(uintptr_t socket, unsigned traffic) {
  auto &api=qwave();
  if(!api.create || !api.add || !api.remove || !api.close) {SetLastError(ERROR_NOT_SUPPORTED);return nullptr;}
  auto *p=new (std::nothrow) sf_tcp_priority{static_cast<SOCKET>(socket)};
  if (!p) {SetLastError(ERROR_NOT_ENOUGH_MEMORY);return nullptr;}
  QOS_VERSION version{1,0};
  if (!api.create(&version,&p->qos) || !sf_network_priority_set(p,traffic)) {
    auto error=GetLastError();delete p;SetLastError(error);return nullptr;
  }
  return p;
}
extern "C" void sf_network_priority_free(void *handle) {delete static_cast<sf_tcp_priority *>(handle);}
