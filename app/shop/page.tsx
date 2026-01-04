import { prisma } from '@/lib/prisma'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { formatCurrency } from '@/lib/utils'
import Link from 'next/link'
import Image from 'next/image'

async function getProducts() {
  try {
    return await prisma.product.findMany({
      where: { active: true },
      orderBy: { createdAt: 'desc' },
      take: 20,
    })
  } catch (error) {
    console.error('Error fetching products:', error)
    return []
  }
}

export default async function ShopPage() {
  const products = await getProducts()

  return (
    <div className="container mx-auto px-4 py-8">
      <div className="mb-8">
        <h1 className="text-4xl font-bold mb-2">Shop</h1>
        <p className="text-gray-400">Browse our selection of game items and services</p>
      </div>

      {products.length === 0 ? (
        <Card className="p-12 text-center">
          <p className="text-gray-400">No products available at the moment.</p>
        </Card>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-6">
          {products.map((product) => (
            <Card key={product.id} className="overflow-hidden hover:border-rust-500 transition-colors">
              {product.image && (
                <div className="relative h-48 w-full bg-gray-700">
                  <Image
                    src={product.image}
                    alt={product.name}
                    fill
                    className="object-cover"
                  />
                </div>
              )}
              <CardHeader>
                <CardTitle className="text-lg">{product.name}</CardTitle>
                <CardDescription className="line-clamp-2">
                  {product.description}
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="flex items-center justify-between">
                  <span className="text-2xl font-bold text-rust-400">
                    {formatCurrency(product.price)}
                  </span>
                  <Button asChild size="sm">
                    <Link href={`/shop/${product.id}`}>View</Link>
                  </Button>
                </div>
                {product.stock > 0 && (
                  <p className="text-sm text-gray-400 mt-2">
                    {product.stock} in stock
                  </p>
                )}
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  )
}

