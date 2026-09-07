// A C++ host for the library. Build the library first — the header lands
// beside it — then compile this against that header and link the library:
//
//   kiln build main.kiln -o lib__MODULE__.so
//   clang++ consumer.cpp -I. -L. -l__MODULE__ -Wl,-rpath,. -o consumer && ./consumer
//
// For Windows, cross-built from Linux (the build writes __MODULE__.dll,
// __MODULE__.h and the import library __MODULE__.lib beside each other):
//
//   kiln build main.kiln --os windows -o __MODULE__.dll
//   cl /EHsc consumer.cpp __MODULE__.lib                        (MSVC, x64)
//   x86_64-w64-mingw32-g++ consumer.cpp -L. -l__MODULE__ -o consumer.exe   (MinGW)
//
// The same file compiles as C (`clang -x c consumer.cpp ...`): the header
// wraps its prototypes in `extern "C"` only when a C++ compiler reads it.
#include <stdio.h>
#include "__MODULE__.h"

#if defined(_WIN32) && defined(_MSC_VER)
#  pragma comment(lib, "__MODULE__.lib")
#endif

int main(void) {
    __MODULE___init();                     /* module variables, once, first */
    greet();
    printf("%d\n", (int)add(2, 3));
    printf("%s\n", greeting("world"));      /* the text belongs to the library */
    printf("greetings so far: %d\n", (int)greetings_so_far());
    return 0;
}
