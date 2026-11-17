// Stream configuration extracted from Apollo v0.4.6 src/video.h, GPL-3.0-only.
// Only the configuration type is needed by the standalone encoder.
#pragma once
namespace video {
  struct config_t {
    int width, height, framerate, bitrate, slicesPerFrame, numRefFrames;
    int encoderCscMode, videoFormat, dynamicRange, chromaSamplingType;
    int enableIntraRefresh, encodingFramerate;
    bool input_only;
  };
}
