// ScreenFerry's C ABI logging adapter for unmodified Apollo NVENC sources.
// SPDX-License-Identifier: GPL-3.0-only
#pragma once
#include <algorithm>
#include <cassert>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <functional>
#include <iomanip>
#include <memory>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

enum severity_level { debug, info, warning, error };
namespace screenferry {
  using log_callback = void (*)(int, const char *);
  inline thread_local log_callback callback = nullptr;
  inline thread_local std::string last_error;
  struct log_line {
    severity_level level;
    std::ostringstream text;
    explicit log_line(severity_level level): level(level) {}
    template<class T> log_line &operator<<(const T &value) { text << value; return *this; }
    ~log_line() noexcept {
      try {
        const auto value = text.str();
        if (level == error) last_error = value;
        if (callback) callback(static_cast<int>(level), value.c_str());
      } catch (...) {}
    }
  };
}
#define BOOST_LOG(level) ::screenferry::log_line(level)
namespace logging {
  template<class T> class min_max_avg_periodic_logger {
    severity_level level;
    std::string label, suffix;
    std::chrono::steady_clock::time_point at = std::chrono::steady_clock::now();
    T minimum{}, maximum{}, sum{};
    uint64_t count = 0;
  public:
    min_max_avg_periodic_logger(severity_level level, const char *label, const char *suffix):
      level(level), label(label), suffix(suffix) {}
    void collect_and_log(T value) {
      minimum = count ? std::min(minimum, value) : value;
      maximum = count ? std::max(maximum, value) : value;
      sum += value; ++count;
      if (std::chrono::steady_clock::now() - at >= std::chrono::seconds(5)) {
        screenferry::log_line(level) << label << ": min=" << minimum << " max=" << maximum << " avg=" << sum / count << suffix;
        count = 0; sum = T{}; at = std::chrono::steady_clock::now();
      }
    }
  };
}
namespace stat_trackers {
  struct decimal_format {
    std::string text;
    template<class T> decimal_format &operator%(T value) {
      std::ostringstream stream; stream << std::fixed << std::setprecision(2) << value; text = stream.str(); return *this;
    }
    friend std::ostream &operator<<(std::ostream &stream, const decimal_format &value) { return stream << value.text; }
  };
  inline decimal_format two_digits_after_decimal() { return {}; }
}
