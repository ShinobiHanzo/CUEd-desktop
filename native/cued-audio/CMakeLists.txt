cmake_minimum_required(VERSION 3.16)
project(cued_audio VERSION 0.2.1 LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_C_STANDARD 99)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

option(CUED_AUDIO_BUILD_CLI "Build the cued-audio command line tool" ON)
option(CUED_AUDIO_BUILD_TESTS "Build the ctest suite" ON)

find_package(Threads REQUIRED)

# ---- library ---------------------------------------------------------------
add_library(cued_audio STATIC
  src/fft.cpp
  src/pcm.cpp
  src/spectrogram.cpp
  src/bpm.cpp
  src/crossfade.cpp
  src/decoder.cpp
  src/analysis.cpp
  src/engine.cpp
  src/error.cpp
  src/cued_audio.cpp
  src/miniaudio_impl.c
)
target_include_directories(cued_audio
  PUBLIC  ${CMAKE_CURRENT_SOURCE_DIR}/include
  PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/src ${CMAKE_CURRENT_SOURCE_DIR}/third_party/miniaudio)
target_link_libraries(cued_audio PUBLIC Threads::Threads ${CMAKE_DL_LIBS} m)
target_compile_options(cued_audio PRIVATE
  $<$<COMPILE_LANGUAGE:CXX>:-Wall -Wextra -Wpedantic>
  # miniaudio is third-party; keep its warnings out of our build log.
  $<$<COMPILE_LANGUAGE:C>:-w>)

# ---- cli -------------------------------------------------------------------
if(CUED_AUDIO_BUILD_CLI)
  add_executable(cued-audio tools/cued_audio_cli.cpp)
  target_link_libraries(cued-audio PRIVATE cued_audio)
  target_compile_options(cued-audio PRIVATE -Wall -Wextra)
endif()

# ---- tests -----------------------------------------------------------------
if(CUED_AUDIO_BUILD_TESTS)
  enable_testing()
  foreach(t fft spectrogram bpm crossfade decoder engine)
    add_executable(test_${t} tests/test_${t}.cpp)
    target_link_libraries(test_${t} PRIVATE cued_audio)
    target_include_directories(test_${t} PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/src)
    target_compile_options(test_${t} PRIVATE -Wall -Wextra)
    add_test(NAME ${t} COMMAND test_${t})
  endforeach()
endif()
