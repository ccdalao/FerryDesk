// ScreenFerry lifecycle/callback binding for the pinned Moonlight RTP queue.
// SPDX-License-Identifier: GPL-3.0-only
#pragma once
#include "upstream/moonlight-common-c/src/Platform.h"
#include "upstream/moonlight-common-c/src/RtpVideoQueue.h"
#undef Limelog
#define Limelog(...) ((void)0)
#define U16(x) ((uint16_t)(x))
#define APP_VERSION_AT_LEAST(a,b,c) (1)
static inline bool isBefore16(uint16_t a, uint16_t b) { return (int16_t)(a-b) < 0; }
extern const int AppVersionQuad[4];
extern __declspec(thread) STREAM_CONFIGURATION StreamConfig;
void queueRtpPacket(PRTPV_QUEUE_ENTRY entry);
void notifyFrameLost(uint32_t frame, bool speculative);
void connectionSawFrame(uint32_t frame);
void connectionSendFrameFecStatus(PSS_FRAME_FEC_STATUS status);
