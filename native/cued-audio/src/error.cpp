#include "error.h"

namespace cued {

namespace {
thread_local std::string g_lastError;
}

void setLastError(const std::string& message) { g_lastError = message; }
const std::string& lastError() { return g_lastError; }

} // namespace cued
